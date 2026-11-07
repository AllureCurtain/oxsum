//! Customer evidence bundles (issue #616, task T25).
//!
//! A usage report answers "what did `endUser` cost me over this span" for a
//! *downstream* customer. An evidence bundle answers the wider question the
//! *organization itself* asks of its operator: "what did this closed period —
//! or this one statement — commit me to, and can I check it offline?" One
//! bundle carries every artifact the scope commits: the usage records settled
//! in the span, the plan statements and invoice journals they billed through,
//! the payment orders and fulfillments that paid them, the refunds, disputes
//! and credit notes that adjusted them, and the day seals that closed the
//! days they booked on.
//!
//! The shape follows the usage report's: a marker entry
//! (`oxsum/evidence-bundle/v1`, a self-balancing 1-minor
//! `Equity:Adjustments` round-trip) commits the *manifest digest* into the
//! tenant's ledger — `H(oxsum/evidence-manifest/v1)` over the canonical
//! manifest, which lists every artifact's own digest. One marker per
//! `(organization, kind, scope)`, ever: the marker's idempotency key and the
//! table's unique index make a re-request replay the standing bundle. The
//! bundle's JSON is stored at mint — bytea, verbatim — so a re-download
//! serves byte-identical bytes however the catalogs have since moved: the
//! digests commit the artifacts as they stood at mint, and the stored copy
//! keeps the two in lockstep the way `usage_report_rows` froze its subset.
//!
//! What a verifier can check: every artifact re-digests to what the manifest
//! names (a tampered row fails its own digest), every artifact the manifest
//! commits is present (a dropped row fails the manifest itself), the ledger
//! entries behind the money artifacts adopt-verify to their recorded content
//! hashes and include in the signed head, and the marker itself proves the
//! manifest is the one the ledger committed. What remains a claim, honestly
//! spelled in the bundle's `scope` block: the platform's own upstream costs
//! (a different organization's books), anything before the span or outside
//! the organization, and whether the operator counted the tokens the model
//! returned.

use std::fmt::Write;

use rand::Rng;
use sha2::{Digest, Sha256};
use sqlx::Row;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;
use crate::wallet::Wallet;
use doubleentry::{Entry, EntryId, Hash};

use crate::wallet::SCALE;

fn invalid(message: &str) -> WalletError {
    WalletError::InvalidInput(message.into())
}

// ── domains, like heads.rs keeps for the older proofs ────────────────────────

/// The document id an evidence-bundle marker's [`doubleentry::DocumentRef`]
/// carries: the bundle's identity, bound to the manifest digest.
pub const EVIDENCE_BUNDLE_DOCUMENT: &str = "oxsum/evidence-bundle/v1";

/// The domain the manifest digest folds under — the canonical manifest bytes,
/// hashed verbatim.
pub const EVIDENCE_MANIFEST_DOMAIN: &[u8] = b"oxsum/evidence-manifest/v1";

/// The domain prefix one artifact's digest hashes under:
/// `H(oxsum/evidence-artifact/v1/<kind>, canonical)` — a usage record's
/// digest cannot be replayed as a statement's.
pub const EVIDENCE_ARTIFACT_DOMAIN: &[u8] = b"oxsum/evidence-artifact/v1/";

/// Every share token starts with this mark, like `oxb-` report links.
const SHARE_TOKEN_MARK: &str = "oxe-";
const SHARE_TOKEN_BYTES: usize = 32;
/// A share's default life, matching report links.
const SHARE_DEFAULT_LIFETIME: Duration = Duration::days(30);
/// The longest expiry a caller may ask for — a share is not a standing grant.
const SHARE_MAX_LIFETIME: Duration = Duration::days(90);
/// The longest share label, matching report links.
const SHARE_MAX_LABEL: usize = 128;
/// The widest day span a period bundle mints: one year and a day, so a
/// mis-typed `from` cannot mint an unbounded scan.
const MAX_PERIOD_DAYS: i64 = 366;

/// A bundle's `scope.covered`/`excluded` block names exactly what the
/// artifacts commit — the honest-scope rule T25 writes into the file itself.
const SCOPE_COVERED: &[&str] = &[
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
const SCOPE_EXCLUDED: &[&str] = &[
    "upstreamCostFigures",
    "otherOrganizationsData",
    "historyOutsideScope",
    "inFlightOperations",
];

/// Which kind of evidence bundle — the marker, the table's `kind`, and the
/// wire all spell the same word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EvidenceKind {
    /// A closed UTC day span's complete-period bundle.
    Period,
    /// One plan statement's focused bundle.
    Statement,
}

impl EvidenceKind {
    /// The spelling `evidence_bundles.kind` stores and the wire carries.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Period => "period",
            Self::Statement => "statement",
        }
    }

    /// Reads a stored kind back.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "period" => Some(Self::Period),
            "statement" => Some(Self::Statement),
            _ => None,
        }
    }
}

/// One artifact's canonical digest: `H(oxsum/evidence-artifact/v1/<kind>,
/// canonical_json)`. The verifier re-digests the served artifact's canonical
/// form and compares — a served row that differs from the committed one fails
/// here before any ledger check runs.
#[must_use]
pub fn evidence_artifact_digest(kind: &str, canonical_json: &str) -> Hash {
    let mut domain = Vec::with_capacity(EVIDENCE_ARTIFACT_DOMAIN.len() + kind.len());
    domain.extend_from_slice(EVIDENCE_ARTIFACT_DOMAIN);
    domain.extend_from_slice(kind.as_bytes());
    Hash::digest(&domain, canonical_json.as_bytes())
}

/// The manifest digest the marker binds: `H(oxsum/evidence-manifest/v1,
/// canonical_manifest_json)`.
#[must_use]
pub fn evidence_manifest_digest(canonical_manifest_json: &str) -> Hash {
    Hash::digest(EVIDENCE_MANIFEST_DOMAIN, canonical_manifest_json.as_bytes())
}

/// One manifest row: an artifact the bundle commits, named by kind and id.
/// The verifier reads `artifacts` as `{kind: {id: json}}` and checks each
/// against its entry here — an artifact missing from the manifest is
/// unattested, an artifact the manifest names but the bundle lacks is a
/// dropped committed row.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestEntry {
    /// The artifact class, `evidence-artifact/v1/<kind>`'s suffix.
    pub kind: String,
    /// The artifact's own id within its class — a request id, a uuid.
    pub id: String,
    /// The canonical digest, lowercase hex.
    pub digest: String,
}

/// The canonical manifest the marker's `DocumentRef` binds — compact JSON in
/// declaration order, hashed verbatim like `canonical_usage_report`.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceManifestV1<'a> {
    /// Canonical-form version.
    pub v: i64,
    /// `period` or `statement`.
    pub kind: &'a str,
    /// The organization's own id — binds the bundle to its customer, so a
    /// bundle minted for one org cannot be replayed as another's.
    pub organization_id: &'a str,
    /// The scope descriptor: what the artifacts commit and what they say they
    /// do not. Part of the committed bytes, so a bundle cannot quietly drop
    /// the honesty.
    pub covered: &'a [&'a str],
    pub excluded: &'a [String],
    /// The committed artifacts, sorted by `(kind, id)` — order is
    /// load-bearing, as it is in the usage-report fold.
    pub entries: &'a [ManifestEntry],
}

/// The bundle the API serves and the verifier parses. Serialized verbatim
/// from the stored bytes — the struct is the wire's own documentation.
/// Serialize-only on this side: the stored `bundle_json` is served verbatim,
/// never re-parsed by the writer, and the verifier's own wire structs do the
/// reading — the same split `ProofBundle`'s `Serialize`-only shape keeps.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceBundle {
    /// Always `2` — the field `verify_bundle`'s legacy shape lacks, and the
    /// version `verify_evidence_bundle` dispatches on.
    pub version: i64,
    /// `period` or `statement`.
    pub kind: String,
    /// The marker entry's id — the bundle's own identity and the ledger
    /// anchor `GET /api/v1/entries/{id}/proof` resolves.
    pub bundle_id: String,
    /// Who the bundle belongs to — the verifier pins the manifest's
    /// `organizationId` to it, and a bundle minted for another org fails here.
    pub organization_id: String,
    /// The manifest digest the marker binds, lowercase hex.
    pub manifest_digest: String,
    /// The covered UTC span for `period`; absent on `statement`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub period_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub period_to: Option<String>,
    /// The statement a `statement` bundle wraps; absent on `period`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub statement_id: Option<String>,
    /// The marker's position in the ledger.
    pub anchor_log_index: i64,
    /// The honest-scope block — the committed claims.
    pub scope: EvidenceScope,
    /// The artifacts, grouped: `{"usageRecord": {"<id>": <json>}, …}` — a
    /// two-level object so a verifier can name the missing row without
    /// index arithmetic.
    pub artifacts: serde_json::Map<String, serde_json::Value>,
    /// Every artifact's manifest rows, `(kind, id)` sorted.
    pub manifest: Vec<ManifestEntry>,
    /// The ledger entries the money artifacts point at — each `{entry,
    /// contentHash, proof}` like a usage-report row, proven against `head`.
    /// The marker itself is `entries[0]`.
    pub entries: Vec<EvidenceEntryBundle>,
    /// The ledger head every entry's proof pins to.
    pub head: doubleentry::TreeHead,
    /// The operator-signed note over `head`, when the deployment signs heads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_hash: Option<String>,
}

/// The scope block a bundle prints — the same arrays the manifest commits.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceScope {
    /// The artifact classes the bundle commits.
    pub covered: Vec<String>,
    /// What it does not attest — each entry a claim class from
    /// `docs/proofs.md`: the platform's supplier costs (an upstream invoice
    /// is the operator's own books), another organization's rows, history
    /// outside the scope, and operations still in flight.
    pub excluded: Vec<String>,
    /// Whether the deployment opted upstream cost figures in — when `false`
    /// the usage artifacts carry `costBasis` but no `upstreamCostMinor`,
    /// and `upstreamCostFigures` stays on the excluded list.
    pub upstream_cost_included: bool,
}

/// One ledger entry in the bundle — the same wire `ReportEntryBundle`
/// carries: the entry's full encoding, its recorded content hash, and its
/// inclusion proof against `head`, plus the `(kind, id)` of the artifact
/// that asked for it.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceEntryBundle {
    /// The entry, balanced — serialized as doubleentry's wire encoding.
    pub entry: Entry<doubleentry::Balanced, { SCALE }>,
    /// The content hash the Merkle log commits, lowercase hex.
    pub content_hash: String,
    /// The entry's inclusion proof into `head`.
    pub proof: doubleentry::InclusionProof,
    /// Which committed artifact this entry evidences — the `(kind, id)` pair
    /// of the manifest row it backs (the marker's own row is `"bundle"` and
    /// the bundle id).
    pub for_kind: String,
    pub for_id: String,
}

/// A minted bundle's descriptor — what `GET /api/v1/org/evidence-bundles`
/// lists; the stored JSON itself is served only by the download.
#[derive(Debug, Clone)]
pub struct EvidenceBundleRow {
    /// The marker entry's id.
    pub bundle_id: Uuid,
    /// The organization it belongs to.
    pub organization_id: Uuid,
    /// `period` or `statement`.
    pub kind: EvidenceKind,
    /// The covered span for `period`.
    pub period_from: Option<time::Date>,
    pub period_to: Option<time::Date>,
    /// The statement a `statement` bundle wraps.
    pub statement_id: Option<Uuid>,
    /// The committed manifest digest, lowercase hex.
    pub manifest_digest: String,
    /// The marker's position in the ledger.
    pub anchor_log_index: i64,
    /// Ledger entries the bundle carries.
    pub entry_count: i64,
    /// When the bundle minted.
    pub created_at: OffsetDateTime,
}

/// A share token's descriptor — the API's list answer, never the token.
#[derive(Debug, Clone)]
pub struct EvidenceShare {
    /// The share's own id, used to revoke it.
    pub share_id: Uuid,
    /// The bundle it opens.
    pub bundle_id: Uuid,
    /// The organization that minted it.
    pub organization_id: Uuid,
    /// The organization's own label for the recipient or purpose.
    pub label: Option<String>,
    /// When it was minted.
    pub created_at: OffsetDateTime,
    /// When it stops resolving.
    pub expires_at: OffsetDateTime,
    /// The remaining download budget at mint; `None` means unbounded until expiry.
    pub max_downloads: Option<i32>,
    /// How many downloads the token has served — the audit figure.
    pub download_count: i32,
    /// When the organization revoked it, if it did.
    pub revoked_at: Option<OffsetDateTime>,
    /// When it last served a download.
    pub last_download_at: Option<OffsetDateTime>,
}

/// A freshly minted share: the only time the token is visible.
#[derive(Debug, Clone)]
pub struct MintedEvidenceShare {
    /// The share's descriptor.
    pub share: EvidenceShare,
    /// The plaintext token, answered once and never stored.
    pub token: String,
}

/// What a presented token resolves to — enough for the public download to
/// serve the bundle without a session.
#[derive(Debug, Clone)]
pub struct ResolvedEvidenceShare {
    /// The bundle the share opens.
    pub bundle_id: Uuid,
    /// The organization the bundle belongs to.
    pub organization_id: Uuid,
    /// The token's own id — the audit trail names it.
    pub share_id: Uuid,
}

/// The scope a mint computes and the bundle reports.
fn evidence_scope(upstream_cost_included: bool) -> EvidenceScope {
    let mut excluded: Vec<String> = SCOPE_EXCLUDED.iter().map(|s| (*s).to_owned()).collect();
    if upstream_cost_included {
        excluded.retain(|s| s != "upstreamCostFigures");
    }
    EvidenceScope {
        covered: SCOPE_COVERED.iter().map(|s| (*s).to_owned()).collect(),
        excluded,
        upstream_cost_included,
    }
}

impl Db {
    /// Mints a period evidence bundle for `organization` over `[from, to]` —
    /// or replays the standing one: the marker's ledger idempotency and the
    /// table's unique index make one bundle per `(organization, span)` ever.
    ///
    /// `to` must be a fully ended UTC day — the same discipline a usage
    /// report applies, because the manifest commits a complete artifact set
    /// and an open day can still book rows the bundle would omit. The span
    /// is bounded to [`MAX_PERIOD_DAYS`] so a mis-typed `from` cannot mint
    /// an unbounded scan.
    ///
    /// `wallet` is the organization's open ledger; `include_upstream_cost`
    /// is the deployment's `OXSUM_EVIDENCE_UPSTREAM_COST` opt-in — off, the
    /// usage artifacts carry no `upstreamCostMinor`. `key` signs the bundle's
    /// head on deployments that sign heads; `None` leaves the signature
    /// fields absent.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] for a malformed span, an open `to`, or
    /// an over-long span; storage and ledger failures surface as
    /// [`WalletError`].
    #[allow(clippy::too_many_arguments)]
    pub async fn mint_period_evidence(
        &self,
        wallet: &Wallet,
        organization_id: Uuid,
        tenant_id: &str,
        from: time::Date,
        to: time::Date,
        include_upstream_cost: bool,
        key: Option<&crate::HeadSigningKey>,
    ) -> Result<EvidenceBundleRow, WalletError> {
        if from > to {
            return Err(invalid("`from` must not be after `to`"));
        }
        if (to - from).whole_days() >= MAX_PERIOD_DAYS {
            return Err(invalid("the span may not exceed 366 days"));
        }
        if to >= OffsetDateTime::now_utc().date() {
            return Err(invalid(
                "`to` must be a fully ended day — a bundle attests a complete period",
            ));
        }

        // A replay reads back the standing row rather than minting a second
        // manifest — the UNIQUE on the scope and the marker's idempotency key
        // are the same rule.
        if let Some(existing) = self
            .evidence_bundle_by_period(organization_id, from, to)
            .await?
        {
            return Ok(existing);
        }

        let mut collector = Collector::new(organization_id, include_upstream_cost);
        self.collect_usage_records(&mut collector, tenant_id, from, to)
            .await?;
        self.collect_plan_statements(&mut collector, organization_id)
            .await?;
        self.collect_orders_and_adjustments(&mut collector, organization_id)
            .await?;
        self.collect_day_seals(&mut collector, organization_id, tenant_id, from, to)
            .await?;
        self.commit_bundle(
            wallet,
            collector,
            EvidenceKind::Period,
            Some(from),
            Some(to),
            None,
            key,
        )
        .await
    }

    /// Mints a statement evidence bundle: the one `plan_statements` row, its
    /// lines, the journal it wraps, and every adjustment/entry that touched it.
    ///
    /// The statement must belong to the organization and be a real document —
    /// `open` rows exist only inside the close transaction, and a foreign or
    /// missing id is `NotFound`, never a cross-customer read.
    ///
    /// # Errors
    ///
    /// [`WalletError::NotFound`] for a statement the organization does not own;
    /// storage and ledger failures surface as [`WalletError`].
    pub async fn mint_statement_evidence(
        &self,
        wallet: &Wallet,
        organization_id: Uuid,
        tenant_id: &str,
        statement_id: Uuid,
        include_upstream_cost: bool,
        key: Option<&crate::HeadSigningKey>,
    ) -> Result<EvidenceBundleRow, WalletError> {
        if let Some(existing) = self
            .evidence_bundle_by_statement(organization_id, statement_id)
            .await?
        {
            return Ok(existing);
        }
        let statement = self
            .plan_statement(organization_id, statement_id)
            .await?
            .ok_or_else(|| WalletError::NotFound("the statement does not exist".into()))?;

        let mut collector = Collector::new(organization_id, include_upstream_cost);
        self.collect_one_statement(&mut collector, &statement)
            .await?;
        self.collect_statement_relations(&mut collector, &statement)
            .await?;
        self.collect_day_seals_for_statement(&mut collector, tenant_id, &statement)
            .await?;
        self.commit_bundle(
            wallet,
            collector,
            EvidenceKind::Statement,
            None,
            None,
            Some(statement_id),
            key,
        )
        .await
    }

    /// The usage rows settled in `[from, to]` — the customer-facing figures
    /// plus the redaction-aware cost fields.
    async fn collect_usage_records(
        &self,
        collector: &mut Collector,
        tenant_id: &str,
        from: time::Date,
        to: time::Date,
    ) -> Result<(), WalletError> {
        let rows = sqlx::query(&format!(
            "SELECT u.request_id, u.entry_id, u.model, u.channel, u.price_version, u.kind, \
                    u.charged_minor, u.freeze_minor, u.input_tokens, u.output_tokens, \
                    u.cached_tokens, u.reasoning_tokens, u.end_user, u.service_tier, \
                    u.event_type, u.cost_basis, u.upstream_cost_minor, u.status, u.settled_at \
             FROM oxsum.usage_records u \
             JOIN ledger_{tenant_id}.entries e ON e.entry_id = u.entry_id \
             WHERE u.tenant_id = $1 AND e.booking_date >= $2 AND e.booking_date <= $3 \
               AND u.status = 'settled' \
             ORDER BY e.log_index"
        ))
        .bind(tenant_id)
        .bind(from)
        .bind(to)
        .fetch_all(self.pool())
        .await?;
        for row in &rows {
            let entry_id: Uuid = row.try_get("entry_id")?;
            let settled_at: OffsetDateTime = row.try_get("settled_at")?;
            let mut artifact = serde_json::json!({
                "v": 1,
                "requestId": row.try_get::<String, _>("request_id")?,
                "entryId": entry_id.to_string(),
                "model": row.try_get::<String, _>("model")?,
                "channel": row.try_get::<String, _>("channel")?,
                "priceVersion": row.try_get::<i64, _>("price_version")?,
                "kind": row.try_get::<String, _>("kind")?,
                "chargedMinor": row.try_get::<i64, _>("charged_minor")?,
                "freezeMinor": row.try_get::<i64, _>("freeze_minor")?,
                "inputTokens": row.try_get::<i64, _>("input_tokens")?,
                "outputTokens": row.try_get::<i64, _>("output_tokens")?,
                "cachedTokens": row.try_get::<i64, _>("cached_tokens")?,
                "reasoningTokens": row.try_get::<i64, _>("reasoning_tokens")?,
                "status": row.try_get::<String, _>("status")?,
                "endUser": row.try_get::<Option<String>, _>("end_user")?,
                "serviceTier": row.try_get::<Option<String>, _>("service_tier")?,
                "eventType": row.try_get::<Option<String>, _>("event_type")?,
                "costBasis": row.try_get::<Option<String>, _>("cost_basis")?,
                "settledAt": settled_at.to_string(),
            });
            // The upstream figure is platform-internal: it joins the canonical
            // bytes only under the deployment's opt-in, so the redacted digest
            // and the opted-in digest honestly differ.
            if collector.include_upstream_cost {
                artifact["upstreamCostMinor"] =
                    serde_json::json!(row.try_get::<Option<i64>, _>("upstream_cost_minor")?);
            }
            let canonical = canonicalize(&artifact)?;
            collector.add(
                "usageRecord",
                row.try_get::<String, _>("request_id")?,
                artifact,
                canonical,
                vec![entry_id],
            );
        }
        Ok(())
    }

    /// Every non-open plan statement the organization carries — for a period
    /// bundle, the documents the span's usage could have billed through.
    async fn collect_plan_statements(
        &self,
        collector: &mut Collector,
        organization_id: Uuid,
    ) -> Result<(), WalletError> {
        let mut after = None;
        loop {
            let page = self.plan_statements(organization_id, after, 100).await?;
            if page.statements.is_empty() {
                break;
            }
            after = page.next_cursor;
            for statement in &page.statements {
                let detail = self
                    .plan_statement(organization_id, statement.statement_id)
                    .await?
                    .ok_or_else(|| invalid("a plan statement vanished mid-collect"))?;
                self.collect_one_statement(collector, &detail).await?;
            }
            if after.is_none() {
                break;
            }
        }
        Ok(())
    }

    /// One statement plus its journal — the artifact's committed unit.
    async fn collect_one_statement(
        &self,
        collector: &mut Collector,
        detail: &crate::PlanStatementDetail,
    ) -> Result<(), WalletError> {
        let journal = match detail.statement.journal_id {
            Some(journal_id) => {
                self.plan_journal(collector.organization_id, journal_id)
                    .await?
            }
            None => None,
        };
        let artifact = serde_json::json!({
            "v": 1,
            "statement": serde_json::to_value(&detail.statement)
                .map_err(|e| invalid(&e.to_string()))?,
            "lines": serde_json::to_value(&detail.lines)
                .map_err(|e| invalid(&e.to_string()))?,
            "journal": journal
                .as_ref()
                .map(|d| serde_json::to_value(&d.journal))
                .transpose()
                .map_err(|e| invalid(&e.to_string()))?,
            "journalLines": journal
                .as_ref()
                .map(|d| serde_json::to_value(&d.lines))
                .transpose()
                .map_err(|e| invalid(&e.to_string()))?,
        });
        collector.add(
            "planStatement",
            detail.statement.statement_id.to_string(),
            artifact.clone(),
            canonicalize(&artifact)?,
            Vec::new(),
        );
        Ok(())
    }

    /// For a statement bundle: the orders, refunds, disputes and credit notes
    /// the statement's lines and journal name — the adjustments a single
    /// statement's evidence must carry.
    async fn collect_statement_relations(
        &self,
        collector: &mut Collector,
        detail: &crate::PlanStatementDetail,
    ) -> Result<(), WalletError> {
        let mut order_ids: Vec<Uuid> = Vec::new();
        if let Some(order_id) = detail.statement.paid_order_id {
            order_ids.push(order_id);
        }
        if let Some(journal_id) = detail.statement.journal_id
            && let Some(journal) = self
                .plan_journal(collector.organization_id, journal_id)
                .await?
            && let Some(order_id) = journal.journal.paid_order_id
        {
            order_ids.push(order_id);
        }
        order_ids.sort_unstable();
        order_ids.dedup();
        for order_id in order_ids {
            self.collect_one_order(collector, order_id).await?;
        }
        Ok(())
    }

    /// Every payment order, refund, dispute and credit note the organization
    /// carries — a period bundle's complete adjustment set.
    async fn collect_orders_and_adjustments(
        &self,
        collector: &mut Collector,
        organization_id: Uuid,
    ) -> Result<(), WalletError> {
        let order_ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT order_id FROM oxsum.payment_orders \
             WHERE organization_id = $1 ORDER BY created_at, order_id",
        )
        .bind(organization_id)
        .fetch_all(self.pool())
        .await?;
        for order_id in order_ids {
            self.collect_one_order(collector, order_id).await?;
        }
        // Orderless adjustments: disputes join through their order, so the
        // orderless set is the org-scoped credit notes that name no order.
        let notes: Vec<sqlx::postgres::PgRow> = sqlx::query(
            "SELECT credit_note_id, organization_id, order_id, refund_id, entry_id, \
                    amount_minor, reason, state, created_at \
             FROM oxsum.credit_notes \
             WHERE organization_id = $1 ORDER BY created_at, credit_note_id",
        )
        .bind(organization_id)
        .fetch_all(self.pool())
        .await?;
        for row in &notes {
            self.collect_credit_note_row(collector, row)?;
        }
        Ok(())
    }

    /// One order plus its fulfillment, refunds, disputes and credit notes —
    /// the money trail a payment left.
    async fn collect_one_order(
        &self,
        collector: &mut Collector,
        order_id: Uuid,
    ) -> Result<(), WalletError> {
        let Some(order) = self
            .payment_order(collector.organization_id, order_id)
            .await?
        else {
            return Ok(());
        };
        let fulfillment = self.payment_fulfillment(order_id).await?;
        let artifact = serde_json::json!({
            "v": 1,
            "order": serde_json::to_value(&order).map_err(|e| invalid(&e.to_string()))?,
            "fulfillment": fulfillment
                .map(|f| serde_json::to_value(&f))
                .transpose()
                .map_err(|e| invalid(&e.to_string()))?,
        });
        collector.add(
            "paymentOrder",
            order_id.to_string(),
            artifact.clone(),
            canonicalize(&artifact)?,
            Vec::new(),
        );

        let refunds = self
            .order_refunds(collector.organization_id, order_id)
            .await?;
        for refund in &refunds {
            self.collect_refund(collector, refund).await?;
        }
        let disputes: Vec<sqlx::postgres::PgRow> = sqlx::query(
            "SELECT dispute_id, order_id, organization_id, provider_dispute_id, \
                    amount_minor, currency, state, recharge_entry_id, created_at, updated_at \
             FROM oxsum.disputes \
             WHERE order_id = $1 AND organization_id = $2 ORDER BY created_at, dispute_id",
        )
        .bind(order_id)
        .bind(collector.organization_id)
        .fetch_all(self.pool())
        .await?;
        for row in &disputes {
            self.collect_dispute_row(collector, row)?;
        }
        Ok(())
    }

    /// One refund — plus the credit note its approval issued, when it has one.
    async fn collect_refund(
        &self,
        collector: &mut Collector,
        refund: &crate::OrderRefund,
    ) -> Result<(), WalletError> {
        let mut entry_ids = Vec::new();
        if let Some(note_id) = refund.note_id
            && let Some(entry_id) = self.credit_note_entry(note_id).await?
        {
            entry_ids.push(entry_id);
        }
        let artifact = serde_json::json!({
            "v": 1,
            "row": serde_json::to_value(refund).map_err(|e| invalid(&e.to_string()))?,
            "entryIds": entry_ids.iter().map(Uuid::to_string).collect::<Vec<_>>(),
        });
        collector.add(
            "refund",
            refund.refund_id.to_string(),
            artifact.clone(),
            canonicalize(&artifact)?,
            entry_ids,
        );
        Ok(())
    }

    /// The credit note's ledger entry — the link a refund's `applied` state
    /// carries when its note landed.
    async fn credit_note_entry(&self, note_id: Uuid) -> Result<Option<Uuid>, WalletError> {
        sqlx::query_scalar("SELECT entry_id FROM oxsum.credit_notes WHERE credit_note_id = $1")
            .bind(note_id)
            .fetch_optional(self.pool())
            .await
            .map(|row| row.flatten())
            .map_err(WalletError::from)
    }

    /// One dispute row — a `lost` one's `recharge_entry_id` is the ledger link.
    fn collect_dispute_row(
        &self,
        collector: &mut Collector,
        row: &sqlx::postgres::PgRow,
    ) -> Result<(), WalletError> {
        let dispute_id: Uuid = row.try_get("dispute_id")?;
        let recharge: Option<Uuid> = row.try_get("recharge_entry_id")?;
        let artifact = serde_json::json!({
            "v": 1,
            "row": {
                "disputeId": dispute_id,
                "orderId": row.try_get::<Uuid, _>("order_id")?,
                "organizationId": row.try_get::<Uuid, _>("organization_id")?,
                "providerDisputeId": row.try_get::<String, _>("provider_dispute_id")?,
                "amountMinor": row.try_get::<i64, _>("amount_minor")?,
                "currency": row.try_get::<String, _>("currency")?,
                "state": row.try_get::<String, _>("state")?,
                "rechargeEntryId": recharge,
                "createdAt": row.try_get::<OffsetDateTime, _>("created_at")?.to_string(),
                "updatedAt": row.try_get::<OffsetDateTime, _>("updated_at")?.to_string(),
            },
            "entryIds": recharge.iter().map(Uuid::to_string).collect::<Vec<_>>(),
        });
        collector.add(
            "dispute",
            dispute_id.to_string(),
            artifact.clone(),
            canonicalize(&artifact)?,
            recharge.into_iter().collect(),
        );
        Ok(())
    }

    /// One credit note row — its `entry_id` is the ledger entry that carries
    /// the credit, the artifact's own proof link.
    fn collect_credit_note_row(
        &self,
        collector: &mut Collector,
        row: &sqlx::postgres::PgRow,
    ) -> Result<(), WalletError> {
        let note_id: Uuid = row.try_get("credit_note_id")?;
        let entry_id: Option<Uuid> = row.try_get("entry_id")?;
        let artifact = serde_json::json!({
            "v": 1,
            "row": {
                "creditNoteId": note_id,
                "organizationId": row.try_get::<Uuid, _>("organization_id")?,
                "orderId": row.try_get::<Option<Uuid>, _>("order_id")?,
                "refundId": row.try_get::<Option<Uuid>, _>("refund_id")?,
                "entryId": entry_id,
                "amountMinor": row.try_get::<i64, _>("amount_minor")?,
                "reason": row.try_get::<String, _>("reason")?,
                "state": row.try_get::<String, _>("state")?,
                "createdAt": row.try_get::<OffsetDateTime, _>("created_at")?.to_string(),
            },
            "entryIds": entry_id.iter().map(Uuid::to_string).collect::<Vec<_>>(),
        });
        collector.add(
            "creditNote",
            note_id.to_string(),
            artifact.clone(),
            canonicalize(&artifact)?,
            entry_id.into_iter().collect(),
        );
        Ok(())
    }

    /// The day seals covering `[from, to]` — the anchored days the span's
    /// usage booked under.
    async fn collect_day_seals(
        &self,
        collector: &mut Collector,
        organization_id: Uuid,
        tenant_id: &str,
        from: time::Date,
        to: time::Date,
    ) -> Result<(), WalletError> {
        // The latest anchor on or after the span's end — the one the
        // completeness proof pins the customer's head to.
        let anchor: Option<(Uuid, Vec<u8>)> = sqlx::query_as(
            "SELECT entry_id, map_root FROM oxsum.usage_anchors \
             WHERE organization_id = $1 AND day >= $2 ORDER BY day LIMIT 1",
        )
        .bind(organization_id)
        .bind(to)
        .fetch_optional(self.pool())
        .await?;
        let Some((anchor_entry, map_root)) = anchor else {
            return Ok(());
        };
        let map_root = Hash::from_bytes(map_root.as_slice().try_into().map_err(|_| {
            WalletError::InvalidInput("the anchor's map root is not 32 bytes".into())
        })?);
        let leaves: Vec<(String, time::Date, Vec<u8>)> = sqlx::query_as(
            "SELECT end_user, day, leaf_hash FROM oxsum.usage_day_log \
             WHERE tenant_id = $1 AND day >= $2 AND day <= $3 ORDER BY end_user, day",
        )
        .bind(tenant_id)
        .bind(from)
        .bind(to)
        .fetch_all(self.pool())
        .await?;
        for (end_user, day, leaf) in leaves {
            let leaf = Hash::from_bytes(leaf.as_slice().try_into().map_err(|_| {
                WalletError::InvalidInput("a stored day leaf is not 32 bytes".into())
            })?);
            let artifact = serde_json::json!({
                "v": 1,
                "day": day.to_string(),
                "endUser": end_user,
                "leafHash": leaf.to_hex(),
                "anchorEntryId": anchor_entry.to_string(),
                "mapRoot": map_root.to_hex(),
            });
            collector.add(
                "daySeal",
                format!("{end_user}/{day}"),
                artifact.clone(),
                canonicalize(&artifact)?,
                vec![anchor_entry],
            );
        }
        Ok(())
    }

    /// The day seals a statement's period covered — `periodStart`/`periodEnd`
    /// on the statement name the span.
    async fn collect_day_seals_for_statement(
        &self,
        collector: &mut Collector,
        tenant_id: &str,
        detail: &crate::PlanStatementDetail,
    ) -> Result<(), WalletError> {
        let (Some(start), Some(end)) = (detail.statement.period_start, detail.statement.period_end)
        else {
            return Ok(());
        };
        self.collect_day_seals(
            collector,
            collector.organization_id,
            tenant_id,
            start.date(),
            end.date(),
        )
        .await
    }

    /// Appends the marker, freezes the bundle, and writes the descriptor row —
    /// the shared tail of both mints. Returns the descriptor the API lists.
    #[allow(clippy::too_many_arguments)]
    async fn commit_bundle(
        &self,
        wallet: &Wallet,
        collector: Collector,
        kind: EvidenceKind,
        period_from: Option<time::Date>,
        period_to: Option<time::Date>,
        statement_id: Option<Uuid>,
        key: Option<&crate::HeadSigningKey>,
    ) -> Result<EvidenceBundleRow, WalletError> {
        let organization_id = collector.organization_id;
        let include_upstream_cost = collector.include_upstream_cost;
        let (mut manifest_entries, artifacts, mut entry_ids) = collector.finish();
        manifest_entries.sort_by(|a, b| {
            (a.kind.as_str(), a.id.as_str()).cmp(&(b.kind.as_str(), b.id.as_str()))
        });

        let excluded: Vec<String> = evidence_scope(include_upstream_cost).excluded;
        let manifest_canonical = serde_json::to_string(&EvidenceManifestV1 {
            v: 1,
            kind: kind.as_str(),
            organization_id: &organization_id.to_string(),
            covered: SCOPE_COVERED,
            excluded: &excluded,
            entries: &manifest_entries,
        })
        .map_err(|e| invalid(&e.to_string()))?;
        let manifest_digest = evidence_manifest_digest(&manifest_canonical);

        // The marker — appended once per scope, like the usage report's. Each
        // kind's companion field is set by construction; a caller that passes
        // one absent is a bug the error names rather than a panic.
        //
        // The scope key carries the organization: the marker's deterministic
        // id doubles as the *global* `oxsum.evidence_bundles` primary key, so
        // two organizations minting the same period span must not collide
        // (a bare `period/<from>/<to>` would map both to one row and the
        // second mint's ON CONFLICT would silently skip, leaving that
        // organization's download a 404).
        let scope_key = match kind {
            EvidenceKind::Period => format!(
                "period/{}/{}/{}",
                organization_id,
                period_from.ok_or_else(|| invalid("a period kind names its span"))?,
                period_to.ok_or_else(|| invalid("a period kind names its span"))?
            ),
            EvidenceKind::Statement => format!(
                "statement/{}/{}",
                organization_id,
                statement_id.ok_or_else(|| invalid("a statement kind names its id"))?
            ),
        };
        let description = serde_json::json!({
            "v": 1,
            "kind": kind.as_str(),
            "organizationId": organization_id,
            "scope": scope_key,
            "manifestDigest": manifest_digest.to_hex(),
            "artifactCount": manifest_entries.len(),
        })
        .to_string();
        let (marker_id, log_index) = wallet
            .anchor_evidence_bundle(&scope_key, &description, manifest_digest)
            .await?;

        // The bundle's entries: the marker first, then every ledger id the
        // artifacts named — deduplicated, each proven against one head.
        entry_ids.sort_unstable();
        entry_ids.dedup();
        let ids: Vec<(EntryId, String, String)> = entry_ids
            .iter()
            .map(|(uuid, (kind, id))| (EntryId::from_uuid(*uuid), kind.clone(), id.clone()))
            .collect();
        let (head, marker, mut entries) = wallet.evidence_entry_bundles(marker_id, &ids).await?;
        entries.insert(0, marker);
        let signed = match key {
            Some(key) => Some(wallet.signed_head_at(key, head.size).await?),
            None => None,
        };
        let (note, key_name, public_key, key_hash) = match signed {
            Some(signed) => (
                Some(signed.note),
                Some(signed.key.name),
                Some(hex_bytes(&signed.key.public_key)),
                Some(hex_bytes(&signed.key.key_hash)),
            ),
            None => (None, None, None, None),
        };

        let bundle = EvidenceBundle {
            version: 2,
            kind: kind.as_str().to_owned(),
            bundle_id: marker_id.as_uuid().to_string(),
            organization_id: organization_id.to_string(),
            manifest_digest: manifest_digest.to_hex(),
            period_from: period_from.map(|d| d.to_string()),
            period_to: period_to.map(|d| d.to_string()),
            statement_id: statement_id.map(|id| id.to_string()),
            anchor_log_index: log_index as i64,
            scope: evidence_scope(include_upstream_cost),
            artifacts,
            manifest: manifest_entries,
            entries,
            head,
            note,
            key_name,
            public_key,
            key_hash,
        };
        let bundle_json = serde_json::to_vec(&bundle).map_err(|e| invalid(&e.to_string()))?;
        let entry_count = bundle.entries.len() as i64;

        sqlx::query(
            "INSERT INTO oxsum.evidence_bundles \
             (bundle_id, organization_id, kind, period_from, period_to, statement_id, \
              manifest_digest, anchor_log_index, bundle_json, entry_count) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
             ON CONFLICT (bundle_id) DO NOTHING",
        )
        .bind(*marker_id.as_uuid())
        .bind(organization_id)
        .bind(kind.as_str())
        .bind(period_from)
        .bind(period_to)
        .bind(statement_id)
        .bind(manifest_digest.to_hex())
        .bind(log_index as i64)
        .bind(&bundle_json)
        .bind(entry_count)
        .execute(self.pool())
        .await?;

        Ok(EvidenceBundleRow {
            bundle_id: *marker_id.as_uuid(),
            organization_id,
            kind,
            period_from,
            period_to,
            statement_id,
            manifest_digest: manifest_digest.to_hex(),
            anchor_log_index: log_index as i64,
            entry_count,
            created_at: OffsetDateTime::now_utc(),
        })
    }

    /// The standing period bundle for `(organization, from, to)` — the replay
    /// path a second mint takes.
    async fn evidence_bundle_by_period(
        &self,
        organization_id: Uuid,
        from: time::Date,
        to: time::Date,
    ) -> Result<Option<EvidenceBundleRow>, WalletError> {
        let row = sqlx::query(
            "SELECT bundle_id, manifest_digest, anchor_log_index, entry_count, created_at \
             FROM oxsum.evidence_bundles \
             WHERE organization_id = $1 AND kind = 'period' \
               AND period_from = $2 AND period_to = $3",
        )
        .bind(organization_id)
        .bind(from)
        .bind(to)
        .fetch_optional(self.pool())
        .await?;
        row.map(|row| {
            Ok(EvidenceBundleRow {
                bundle_id: row.try_get("bundle_id")?,
                organization_id,
                kind: EvidenceKind::Period,
                period_from: Some(from),
                period_to: Some(to),
                statement_id: None,
                manifest_digest: row.try_get("manifest_digest")?,
                anchor_log_index: row.try_get("anchor_log_index")?,
                entry_count: row.try_get("entry_count")?,
                created_at: row.try_get("created_at")?,
            })
        })
        .transpose()
    }

    /// The standing statement bundle — a statement's evidence mints once.
    async fn evidence_bundle_by_statement(
        &self,
        organization_id: Uuid,
        statement_id: Uuid,
    ) -> Result<Option<EvidenceBundleRow>, WalletError> {
        let row = sqlx::query(
            "SELECT bundle_id, manifest_digest, anchor_log_index, entry_count, created_at \
             FROM oxsum.evidence_bundles \
             WHERE organization_id = $1 AND kind = 'statement' AND statement_id = $2",
        )
        .bind(organization_id)
        .bind(statement_id)
        .fetch_optional(self.pool())
        .await?;
        row.map(|row| {
            Ok(EvidenceBundleRow {
                bundle_id: row.try_get("bundle_id")?,
                organization_id,
                kind: EvidenceKind::Statement,
                period_from: None,
                period_to: None,
                statement_id: Some(statement_id),
                manifest_digest: row.try_get("manifest_digest")?,
                anchor_log_index: row.try_get("anchor_log_index")?,
                entry_count: row.try_get("entry_count")?,
                created_at: row.try_get("created_at")?,
            })
        })
        .transpose()
    }

    /// A bundle's descriptor by id — `None` when the id is no bundle or not
    /// the organization's.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn evidence_bundle(
        &self,
        organization_id: Uuid,
        bundle_id: Uuid,
    ) -> Result<Option<EvidenceBundleRow>, WalletError> {
        let row = sqlx::query(
            "SELECT kind, period_from, period_to, statement_id, manifest_digest, \
                    anchor_log_index, entry_count, created_at \
             FROM oxsum.evidence_bundles \
             WHERE bundle_id = $1 AND organization_id = $2",
        )
        .bind(bundle_id)
        .bind(organization_id)
        .fetch_optional(self.pool())
        .await?;
        row.map(|row| {
            let kind = EvidenceKind::parse(&row.try_get::<String, _>("kind")?)
                .ok_or_else(|| WalletError::Misconfigured("unknown bundle kind".into()))?;
            Ok(EvidenceBundleRow {
                bundle_id,
                organization_id,
                kind,
                period_from: row.try_get("period_from")?,
                period_to: row.try_get("period_to")?,
                statement_id: row.try_get("statement_id")?,
                manifest_digest: row.try_get("manifest_digest")?,
                anchor_log_index: row.try_get("anchor_log_index")?,
                entry_count: row.try_get("entry_count")?,
                created_at: row.try_get("created_at")?,
            })
        })
        .transpose()
    }

    /// The frozen bundle bytes — what the owner download and the share link
    /// both serve verbatim.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn evidence_bundle_json(
        &self,
        bundle_id: Uuid,
    ) -> Result<Option<Vec<u8>>, WalletError> {
        sqlx::query_scalar("SELECT bundle_json FROM oxsum.evidence_bundles WHERE bundle_id = $1")
            .bind(bundle_id)
            .fetch_optional(self.pool())
            .await
            .map_err(WalletError::from)
    }

    /// The organization's minted bundles, newest first — descriptors only.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn evidence_bundles_of(
        &self,
        organization_id: Uuid,
    ) -> Result<Vec<EvidenceBundleRow>, WalletError> {
        let rows = sqlx::query(
            "SELECT bundle_id, kind, period_from, period_to, statement_id, \
                    manifest_digest, anchor_log_index, entry_count, created_at \
             FROM oxsum.evidence_bundles WHERE organization_id = $1 \
             ORDER BY created_at DESC",
        )
        .bind(organization_id)
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|row| {
                let kind = EvidenceKind::parse(&row.try_get::<String, _>("kind")?)
                    .ok_or_else(|| WalletError::Misconfigured("unknown bundle kind".into()))?;
                Ok(EvidenceBundleRow {
                    bundle_id: row.try_get("bundle_id")?,
                    organization_id,
                    kind,
                    period_from: row.try_get("period_from")?,
                    period_to: row.try_get("period_to")?,
                    statement_id: row.try_get("statement_id")?,
                    manifest_digest: row.try_get("manifest_digest")?,
                    anchor_log_index: row.try_get("anchor_log_index")?,
                    entry_count: row.try_get("entry_count")?,
                    created_at: row.try_get("created_at")?,
                })
            })
            .collect()
    }

    /// Mints a share token for a bundle the organization owns. The token is
    /// answered once; only its SHA-256 is stored.
    ///
    /// # Errors
    ///
    /// [`WalletError::NotFound`] for a bundle the organization does not own;
    /// [`WalletError::InvalidInput`] for a label, expiry or download bound
    /// outside the limits.
    pub async fn create_evidence_share(
        &self,
        organization_id: Uuid,
        bundle_id: Uuid,
        label: Option<&str>,
        expires_at: Option<OffsetDateTime>,
        max_downloads: Option<i32>,
        created_by: Option<Uuid>,
    ) -> Result<MintedEvidenceShare, WalletError> {
        let label = label.map(str::trim).filter(|text| !text.is_empty());
        if let Some(label) = label
            && label.chars().count() > SHARE_MAX_LABEL
        {
            return Err(invalid("label must be at most 128 characters"));
        }
        let now = OffsetDateTime::now_utc();
        let expires_at = expires_at.unwrap_or(now + SHARE_DEFAULT_LIFETIME);
        if expires_at <= now {
            return Err(invalid("expiresAt must be in the future"));
        }
        if expires_at > now + SHARE_MAX_LIFETIME {
            return Err(invalid("expiresAt may not be more than 90 days out"));
        }
        if let Some(max) = max_downloads
            && max <= 0
        {
            return Err(invalid("maxDownloads must be positive"));
        }

        let share_id = Uuid::new_v4();
        let token = generate_share_token();
        let created_at: OffsetDateTime = sqlx::query_scalar(
            "INSERT INTO oxsum.evidence_share_tokens \
                 (share_id, bundle_id, organization_id, token_hash, label, \
                  expires_at, max_downloads, created_by) \
             SELECT $1, b.bundle_id, b.organization_id, $2, $3, $4, $5, $6 \
             FROM oxsum.evidence_bundles b \
             WHERE b.bundle_id = $7 AND b.organization_id = $8 \
             RETURNING created_at",
        )
        .bind(share_id)
        .bind(hash_share_token(&token).as_slice())
        .bind(label)
        .bind(expires_at)
        .bind(max_downloads)
        .bind(created_by)
        .bind(bundle_id)
        .bind(organization_id)
        .fetch_optional(self.pool())
        .await?
        .ok_or_else(|| WalletError::NotFound("the bundle does not exist".into()))?;

        Ok(MintedEvidenceShare {
            share: EvidenceShare {
                share_id,
                bundle_id,
                organization_id,
                label: label.map(str::to_owned),
                created_at,
                expires_at,
                max_downloads,
                download_count: 0,
                revoked_at: None,
                last_download_at: None,
            },
            token,
        })
    }

    /// The shares a bundle carries — descriptors only, newest first.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn evidence_shares_of(
        &self,
        organization_id: Uuid,
        bundle_id: Uuid,
    ) -> Result<Vec<EvidenceShare>, WalletError> {
        let rows = sqlx::query(
            "SELECT share_id, bundle_id, label, created_at, expires_at, max_downloads, \
                    download_count, revoked_at, last_download_at \
             FROM oxsum.evidence_share_tokens \
             WHERE organization_id = $1 AND bundle_id = $2 ORDER BY created_at DESC",
        )
        .bind(organization_id)
        .bind(bundle_id)
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|row| {
                Ok(EvidenceShare {
                    share_id: row.try_get("share_id")?,
                    bundle_id: row.try_get("bundle_id")?,
                    organization_id,
                    label: row.try_get("label")?,
                    created_at: row.try_get("created_at")?,
                    expires_at: row.try_get("expires_at")?,
                    max_downloads: row.try_get("max_downloads")?,
                    download_count: row.try_get("download_count")?,
                    revoked_at: row.try_get("revoked_at")?,
                    last_download_at: row.try_get("last_download_at")?,
                })
            })
            .collect()
    }

    /// Revokes every live share on a bundle — the `DELETE` the API maps; an
    /// already-revoked share is idempotent. Returns how many live shares the
    /// call revoked.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn revoke_evidence_shares(
        &self,
        organization_id: Uuid,
        bundle_id: Uuid,
    ) -> Result<u64, WalletError> {
        let result = sqlx::query(
            "UPDATE oxsum.evidence_share_tokens SET revoked_at = now() \
             WHERE organization_id = $1 AND bundle_id = $2 AND revoked_at IS NULL",
        )
        .bind(organization_id)
        .bind(bundle_id)
        .execute(self.pool())
        .await?;
        Ok(result.rows_affected())
    }

    /// Resolves a presented token, consuming one download of its budget.
    /// Expired, revoked, exhausted and unknown tokens all answer `None` —
    /// the caller cannot tell them apart, and a raced download cannot spend
    /// the same remaining budget twice because the check and the increment
    /// are one `UPDATE`.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn resolve_evidence_share(
        &self,
        token: &str,
    ) -> Result<Option<ResolvedEvidenceShare>, WalletError> {
        let row = sqlx::query(
            "UPDATE oxsum.evidence_share_tokens \
             SET download_count = download_count + 1, last_download_at = now() \
             WHERE token_hash = $1 AND revoked_at IS NULL AND expires_at > now() \
               AND (max_downloads IS NULL OR download_count < max_downloads) \
             RETURNING share_id, bundle_id, organization_id",
        )
        .bind(hash_share_token(token).as_slice())
        .fetch_optional(self.pool())
        .await?;
        row.map(|row| {
            Ok(ResolvedEvidenceShare {
                share_id: row.try_get("share_id")?,
                bundle_id: row.try_get("bundle_id")?,
                organization_id: row.try_get("organization_id")?,
            })
        })
        .transpose()
    }
}

/// What a mint collects: the manifest plus the artifact map, gathered under
/// one organization. `entry_ids` pairs each ledger entry id the artifacts
/// named with the `(kind, id)` of the artifact that wants its proof.
struct Collector {
    organization_id: Uuid,
    include_upstream_cost: bool,
    manifest: Vec<ManifestEntry>,
    artifacts: serde_json::Map<String, serde_json::Value>,
    entry_ids: Vec<(Uuid, (String, String))>,
}

impl Collector {
    fn new(organization_id: Uuid, include_upstream_cost: bool) -> Self {
        Self {
            organization_id,
            include_upstream_cost,
            manifest: Vec::new(),
            artifacts: serde_json::Map::new(),
            entry_ids: Vec::new(),
        }
    }

    /// Records one artifact: its digest joins the manifest, its canonical
    /// value joins the artifact map, and every entry id the artifact named
    /// joins the proof list under this artifact's `(kind, id)`.
    fn add(
        &mut self,
        kind: &str,
        id: String,
        value: serde_json::Value,
        canonical: String,
        entry_ids: Vec<Uuid>,
    ) {
        let digest = evidence_artifact_digest(kind, &canonical);
        self.manifest.push(ManifestEntry {
            kind: kind.to_owned(),
            id: id.clone(),
            digest: digest.to_hex(),
        });
        // The bucket is always an object — it is created as `json!({})` and
        // only ever has artifacts inserted — so the entry lands without
        // re-checking; a `Value` that is somehow not an object replaces it.
        let bucket = self
            .artifacts
            .entry(kind.to_owned())
            .or_insert_with(|| serde_json::json!({}));
        if !bucket.is_object() {
            *bucket = serde_json::json!({});
        }
        if let serde_json::Value::Object(bucket) = bucket {
            bucket.insert(id.clone(), value);
        }
        for entry_id in entry_ids {
            self.entry_ids
                .push((entry_id, (kind.to_owned(), id.clone())));
        }
    }

    /// Answers the collected halves: manifest rows, the artifact map, and the
    /// entry list — deduplicated. Sorting is the caller's, since the manifest
    /// order is committed into the digest.
    fn finish(self) -> Collected {
        (self.manifest, self.artifacts, self.entry_ids)
    }
}

/// What [`Collector::finish`] hands back: the manifest rows, the `{kind:
/// {id: json}}` artifact map, and the `(entry_id, (kind, id))` proof list.
type Collected = (
    Vec<ManifestEntry>,
    serde_json::Map<String, serde_json::Value>,
    Vec<(Uuid, (String, String))>,
);

/// Canonicalizes one artifact's bytes: `serde_json`'s own object ordering is
/// already deterministic for `Value` (BTreeMap-ordered keys), so re-parsing
/// the value and re-serializing gives the exact bytes the verifier will
/// produce from the served JSON — the digest binds the *value*, not this
/// writer's incidental key order.
fn canonicalize(value: &serde_json::Value) -> Result<String, WalletError> {
    serde_json::to_string(value).map_err(|e| invalid(&e.to_string()))
}

/// Lowercase hex of a byte slice — the spelling the bundle's key fields carry.
fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A new share token: the mark plus 32 random bytes from the operating
/// system's CSPRNG, hex-encoded like an API key secret.
fn generate_share_token() -> String {
    let mut bytes = [0u8; SHARE_TOKEN_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    let mut token = String::with_capacity(SHARE_TOKEN_MARK.len() + SHARE_TOKEN_BYTES * 2);
    token.push_str(SHARE_TOKEN_MARK);
    for byte in bytes {
        let _ = write!(token, "{byte:02x}");
    }
    token
}

/// SHA-256 of the token, the only form the database ever holds.
fn hash_share_token(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}
