//! Ledger anchors, evidence bundles, signed heads and refusal records.

use crate::billing::{Settlement, SettlementKind, SettlementRecord};
use crate::error::{WalletError, invalid};
use crate::evidence::{EVIDENCE_BUNDLE_DOCUMENT, EvidenceEntryBundle};
use crate::heads::{
    Consistency, HeadSigningKey, STATEMENT_ANCHOR_DOCUMENT, SignedHead, USAGE_ANCHOR_DOCUMENT,
    USAGE_REPORT_DOCUMENT, canonical_price_row, origin_for, sign_head, sign_price_head,
    sign_sale_checkpoint,
};
use crate::proof::ProofBundle;
use crate::usage::UsageRecord;
use crate::usage_reports::{ReportEntryBundle, UsageReportRow};
use doubleentry::{
    Direction, DocumentRef, Draft, Entry, EntryId, Hash, Layer, Provenance, StoredEntry,
};

use super::{Credits, SCALE, Wallet, currency, description_of, entry_id_for, idem, stored_receipt};
use doubleentry::LedgerStore;
use time::OffsetDateTime;
use uuid::Uuid;

/// Lowercase hex of a byte slice, the spelling the signed-link key fields use.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl Wallet {
    /// Appends the statement anchor — the marker entry committing a finalized
    /// statement's digests into the tenant ledger (issue #221). `pub(crate)`:
    /// only [`crate::Db::finalize_statement`] calls it.
    ///
    /// One ledger entry per statement: a 1-minor balanced `Equity:Adjustments`
    /// debit/credit (the posting amount is immaterial — the marker is metadata,
    /// not money), idempotency-keyed `statement-anchor/<statement_id>` so a
    /// retried finalize replays rather than duplicates. The entry's
    /// [`DocumentRef`] binds the marker to the window digest, and its
    /// description carries the rollup digest plus the figures it summarizes.
    /// Returns the marker's id and its position in the log — the head that
    /// follows that position is what proves the anchor was committed.
    ///
    /// # Errors
    ///
    /// A retained marker is replayed only when its exact descriptor, document
    /// binding and balanced metadata postings match. A legacy orphan with
    /// different source inputs requires explicit recovery; it is never adopted.
    /// Storage and ledger failures surface as [`WalletError`].
    pub(crate) async fn anchor_statement_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        statement_id: Uuid,
        description_json: &str,
        window_hash: Hash,
    ) -> Result<(EntryId, u64), WalletError> {
        self.lock_append_in(tx).await?;
        let key = format!("statement-anchor/{statement_id}");
        let receipt = if let Some(stored) = self.entry_in(tx, entry_id_for(&key)).await? {
            let postings = stored.entry.postings();
            let marker = postings.len() == 2
                && postings.iter().all(|p| {
                    p.account == self.adjustments
                        && p.amount.to_minor() == 1
                        && p.currency == currency()
                        && p.layer == Layer::Settled
                        && p.dimensions.is_empty()
                })
                && stored.entry.kind().is_none()
                && stored.entry.provenance() == &Provenance::none()
                && stored.entry.reverses().is_none()
                && stored.entry.original_booking_date().is_none()
                && stored.entry.value_date() == stored.entry.booking_date()
                && postings[0].direction == Direction::Debit
                && postings[1].direction == Direction::Credit;
            let exact = marker
                && stored.entry.idempotency_key().as_bytes() == key.as_bytes()
                && stored.entry.description().as_str() == description_json
                && stored.entry.document().is_some_and(|document| {
                    document.id.as_str() == STATEMENT_ANCHOR_DOCUMENT
                        && document.content_hash == Some(window_hash)
                });
            if !exact {
                return Err(WalletError::Conflict("retained statement anchor names different source inputs; explicit recovery required".into()));
            }
            stored_receipt(&stored)
        } else {
            let minor = Credits::from_minor(1);
            let entry = Entry::<Draft, SCALE>::new(
                entry_id_for(&key),
                idem(&key)?,
                OffsetDateTime::now_utc().date(),
            )
            .with_document(
                DocumentRef::new(STATEMENT_ANCHOR_DOCUMENT, window_hash).map_err(invalid)?,
            )
            .with_description(description_of(description_json)?)
            .debit(self.adjustments, minor, currency())
            .credit(self.adjustments, minor, currency());
            self.append_in(tx, entry).await?
        };
        let index = receipt.log_index.ok_or_else(|| {
            WalletError::InvalidInput("the statement anchor did not sequence".into())
        })?;
        Ok((receipt.entry_id, index))
    }
    /// Appends a usage-report marker — the entry that commits an end-user's
    /// subset into the tenant ledger (issue #261). `pub(crate)`: only
    /// [`crate::Db::mint_usage_report`] calls it.
    ///
    /// One ledger entry per `(end_user, from, to)`: a 1-minor balanced
    /// `Equity:Adjustments` debit/credit (the posting amount is immaterial —
    /// the marker is metadata, not money), idempotency-keyed
    /// `usage-report/<endUser>/<from>/<to>` so a re-request for the same span
    /// replays the standing marker rather than committing a second subset —
    /// the once-per-span rule the completeness claim rests on. The entry's
    /// [`DocumentRef`] binds the marker to the subset digest, and its
    /// description carries the canonical commitment.
    ///
    /// # Errors
    ///
    /// Storage and ledger failures surface as [`WalletError`].
    pub(crate) async fn anchor_usage_report(
        &self,
        end_user: &str,
        from: &str,
        to: &str,
        description_json: &str,
        subset_hash: Hash,
    ) -> Result<(EntryId, u64), WalletError> {
        let key = format!("usage-report/{end_user}/{from}/{to}");
        let minor = Credits::from_minor(1);
        let entry = Entry::<Draft, SCALE>::new(
            entry_id_for(&key),
            idem(&key)?,
            OffsetDateTime::now_utc().date(),
        )
        .with_document(DocumentRef::new(USAGE_REPORT_DOCUMENT, subset_hash).map_err(invalid)?)
        .with_description(description_of(description_json)?)
        .debit(self.adjustments, minor, currency())
        .credit(self.adjustments, minor, currency());
        let receipt = match self.append(entry).await {
            Ok(receipt) => receipt,
            Err(error) => {
                // A raced mint lands on the same idempotency key; the standing
                // entry is the same report either way, so read it back.
                let Some(stored) = self.store.get(entry_id_for(&key)).await? else {
                    return Err(error);
                };
                stored_receipt(&stored)
            }
        };
        let index = receipt.log_index.ok_or_else(|| {
            WalletError::InvalidInput("the usage-report marker did not sequence".into())
        })?;
        Ok((receipt.entry_id, index))
    }
    /// Appends a usage day anchor — the marker entry committing `day`'s
    /// sorted-map root into the tenant ledger (issue #338).
    ///
    /// One entry per `(organization, day)`: a 1-minor balanced
    /// `Equity:Adjustments` debit/credit (the marker is metadata, not money),
    /// idempotency-keyed `usage-anchor/<day>` so a retried pass replays the
    /// standing entry instead of committing a second root for the same day. The
    /// [`DocumentRef`] binds the anchor to the map root, and the description
    /// carries the day, root and customer count.
    fn anchor_usage_draft(
        &self,
        day: time::Date,
        description_json: &str,
        map_root: Hash,
    ) -> Result<Entry<Draft, SCALE>, WalletError> {
        let key = format!("usage-anchor/{day}");
        let minor = Credits::from_minor(1);
        Ok(Entry::<Draft, SCALE>::new(
            entry_id_for(&key),
            idem(&key)?,
            OffsetDateTime::now_utc().date(),
        )
        .with_document(DocumentRef::new(USAGE_ANCHOR_DOCUMENT, map_root).map_err(invalid)?)
        .with_description(description_of(description_json)?)
        .debit(self.adjustments, minor, currency())
        .credit(self.adjustments, minor, currency()))
    }
    /// Appends `day`'s usage anchor inside the caller's transaction
    /// (issue #1088): the pass reads the day's leaves and inserts the
    /// bookkeeping row on the same transaction, so the committed `map_root`
    /// provably covers every leaf the day holds — a late settle either holds
    /// the per-customer advisory lock this transaction already took, or blocks
    /// on it until the anchor commits, where `day_is_anchored_in` then refuses
    /// it.
    ///
    /// The entry carries the same idempotency key as the standalone form, so
    /// a replay inside the transaction answers the standing entry rather than
    /// conflicting on it.
    ///
    /// # Errors
    ///
    /// As [`append_in`](Self::append_in); the transaction is the caller's to
    /// commit or roll back.
    pub(crate) async fn anchor_usage_day_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        day: time::Date,
        description_json: &str,
        map_root: Hash,
    ) -> Result<(EntryId, u64), WalletError> {
        let key = format!("usage-anchor/{day}");
        let entry = self.anchor_usage_draft(day, description_json, map_root)?;
        let receipt = match self.append_in(tx, entry).await {
            Ok(receipt) => receipt,
            Err(error) => {
                // A raced pass lands on the same idempotency key; the standing
                // entry is the same anchor either way, so read it back on the
                // caller's transaction rather than committing a second root.
                let Some(stored) = self.entry_in(tx, entry_id_for(&key)).await? else {
                    return Err(error);
                };
                stored_receipt(&stored)
            }
        };
        let index = receipt
            .log_index
            .ok_or_else(|| WalletError::InvalidInput("the usage anchor did not sequence".into()))?;
        Ok((receipt.entry_id, index))
    }
    /// Assembles a usage report's self-contained bundle under one head (issue
    /// #261): the marker entry, every shown settlement entry, each with its
    /// inclusion proof, all proven against the *same* head — then that head
    /// signed when the deployment signs heads.
    ///
    /// The single head is the load-bearing detail. Proving each row against
    /// the current head as it is read would let a concurrent append hand the
    /// bundle two different heads, and a row's proof would fail against the
    /// head the report actually attests. Here the head is read once and every
    /// `prove_inclusion_at` pins to its size, so the bundle is internally
    /// consistent even while the ledger grows.
    ///
    /// `rows` are the frozen `usage_report_rows` (or a fresh mint's subset): each
    /// carries `entry_id`, `log_index` and the recorded `content_hash`. The
    /// `entry` object is read back from the ledger by id — the bundle serves the
    /// entry's full serde encoding so the verifier recomputes the hash rather
    /// than trusting a served one. A row whose id no longer resolves, or whose
    /// recomputed hash disagrees with what was frozen, is a data-integrity
    /// failure and surfaces as [`WalletError::InvalidInput`].
    ///
    /// Returns `(marker_bundle, entry_bundles, head, signed_head)`; the route
    /// serializes them into the `UsageReport` body. `signed_head` is `None` on a
    /// deployment that does not sign heads — the bundle is then verified to the
    /// digests alone, the same posture `receipt_proof` takes with `price`.
    ///
    /// # Errors
    ///
    /// Storage and ledger failures surface as [`WalletError`].
    pub async fn usage_report_bundle(
        &self,
        marker_id: EntryId,
        rows: &[UsageReportRow],
        key: Option<&HeadSigningKey>,
    ) -> Result<
        (
            ReportEntryBundle,
            Vec<ReportEntryBundle>,
            doubleentry::TreeHead,
            Option<SignedHead>,
        ),
        WalletError,
    > {
        let head = self.store.head().await?;
        // The marker is identified by id and reports its own hash; the shown
        // entries additionally pin the hash frozen at mint, so a ledger row
        // altered underneath is reported rather than served.
        let Some(stored) = self.store.get(marker_id).await? else {
            return Err(WalletError::InvalidInput(
                "the usage-report marker is gone from the ledger".into(),
            ));
        };
        let marker = ReportEntryBundle {
            proof: self
                .store
                .prove_inclusion_at(stored.require_index().map_err(invalid)?, head.size)
                .await
                .map_err(invalid)?,
            content_hash: stored.content_hash.to_hex(),
            entry: stored.entry,
        };
        let mut entries = Vec::with_capacity(rows.len());
        for row in rows {
            let Some(stored) = self.store.get(EntryId::from_uuid(row.entry_id)).await? else {
                return Err(WalletError::InvalidInput(
                    "a usage-report row's entry is gone from the ledger".into(),
                ));
            };
            if stored.content_hash.to_hex() != row.content_hash {
                return Err(WalletError::InvalidInput(
                    "a usage-report row's hash disagrees with the ledger".into(),
                ));
            }
            entries.push(ReportEntryBundle {
                proof: self
                    .store
                    .prove_inclusion_at(stored.require_index().map_err(invalid)?, head.size)
                    .await
                    .map_err(invalid)?,
                content_hash: stored.content_hash.to_hex(),
                entry: stored.entry,
            });
        }
        let signed = match key {
            Some(key) => {
                let origin = origin_for(&self.tenant_id).map_err(invalid)?;
                Some(sign_head(key, &origin, head).map_err(invalid)?)
            }
            None => None,
        };
        Ok((marker, entries, head, signed))
    }
    /// One entry's bundle part — the entry, its recorded hash and its inclusion
    /// proof against `head` — for callers that need a marker-like entry outside
    /// a report's row list (the daily usage anchor, issue #339).
    pub(crate) async fn entry_bundle(
        &self,
        entry_id: EntryId,
        head: doubleentry::TreeHead,
    ) -> Result<ReportEntryBundle, WalletError> {
        let Some(stored) = self.store.get(entry_id).await? else {
            return Err(WalletError::InvalidInput(
                "the anchored entry is gone from the ledger".into(),
            ));
        };
        Ok(ReportEntryBundle {
            proof: self
                .store
                .prove_inclusion_at(stored.require_index().map_err(invalid)?, head.size)
                .await
                .map_err(invalid)?,
            content_hash: stored.content_hash.to_hex(),
            entry: stored.entry,
        })
    }
    /// Appends an evidence-bundle marker — the entry that commits a customer
    /// evidence bundle's manifest digest into the tenant ledger (issue #616).
    /// `pub(crate)`: only [`crate::Db::mint_period_evidence`] and
    /// [`crate::Db::mint_statement_evidence`] call it.
    ///
    /// One ledger entry per `(kind, scope)`: a 1-minor balanced
    /// `Equity:Adjustments` debit/credit (the posting amount is immaterial —
    /// the marker is metadata, not money), idempotency-keyed
    /// `evidence-bundle/<scope>` so a re-request for the same span or
    /// statement replays the standing marker rather than committing a second
    /// manifest — the once-per-scope rule the manifest's completeness claim
    /// rests on. The entry's [`DocumentRef`] binds the marker to the manifest
    /// digest, and its description carries the scope and figures.
    ///
    /// # Errors
    ///
    /// Storage and ledger failures surface as [`WalletError`].
    pub(crate) async fn anchor_evidence_bundle(
        &self,
        scope_key: &str,
        description_json: &str,
        manifest_digest: Hash,
    ) -> Result<(EntryId, u64), WalletError> {
        let key = format!("evidence-bundle/{scope_key}");
        let minor = Credits::from_minor(1);
        let entry = Entry::<Draft, SCALE>::new(
            entry_id_for(&key),
            idem(&key)?,
            OffsetDateTime::now_utc().date(),
        )
        .with_document(
            DocumentRef::new(EVIDENCE_BUNDLE_DOCUMENT, manifest_digest).map_err(invalid)?,
        )
        .with_description(description_of(description_json)?)
        .debit(self.adjustments, minor, currency())
        .credit(self.adjustments, minor, currency());
        // Read an existing exact marker before sealing today's draft, so a
        // retry after calendar closure remains recoverable. Other append errors
        // propagate; only an actual idempotency conflict gets a raced reread.
        let valid = |stored: &StoredEntry<SCALE>| {
            stored.entry.document().is_some_and(|document| {
                document.id.as_str() == EVIDENCE_BUNDLE_DOCUMENT
                    && document.content_hash == Some(manifest_digest)
            })
        };
        let receipt = if let Some(stored) = self.store.get(entry_id_for(&key)).await? {
            if !valid(&stored) {
                return Err(WalletError::Conflict("retained evidence marker names different captured inputs; explicit recovery required".into()));
            }
            stored_receipt(&stored)
        } else {
            match self.append(entry).await {
                Ok(receipt) => receipt,
                Err(error @ WalletError::Conflict(_)) => {
                    let Some(stored) = self.store.get(entry_id_for(&key)).await? else {
                        return Err(error);
                    };
                    if !valid(&stored) {
                        return Err(WalletError::Conflict("retained evidence marker names different captured inputs; explicit recovery required".into()));
                    }
                    stored_receipt(&stored)
                }
                Err(error) => return Err(error),
            }
        };
        let index = receipt.log_index.ok_or_else(|| {
            WalletError::InvalidInput("the evidence marker did not sequence".into())
        })?;
        Ok((receipt.entry_id, index))
    }
    /// The marker plus every entry `ids` names, each proven against the one
    /// head — the `entries` array an evidence bundle serializes (issue #616).
    /// Proving every row against a single read head keeps the bundle
    /// internally consistent while the ledger grows, the same discipline
    /// `usage_report_bundle` keeps.
    ///
    /// `ids` pairs each entry id with the `(kind, id)` of the artifact that
    /// wants its proof; the marker comes first, tagged `("bundle", its id)`.
    /// `signed_head` — `signed_head_at(head.size)` under the caller's key —
    /// is left to the caller, which already holds the optional key.
    ///
    /// Returns `(head, marker, entries)` in the bundle's own order.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] when an id no longer resolves, or a stored
    /// entry's hash disagrees with what the artifact committed; storage and
    /// ledger failures surface as [`WalletError`].
    pub(crate) async fn evidence_entry_bundles(
        &self,
        marker_id: EntryId,
        ids: &[(EntryId, String, String)],
    ) -> Result<
        (
            doubleentry::TreeHead,
            EvidenceEntryBundle,
            Vec<EvidenceEntryBundle>,
        ),
        WalletError,
    > {
        let head = self.store.head().await?;
        let marker = self.entry_bundle(marker_id, head).await?;
        let marker = EvidenceEntryBundle {
            entry: marker.entry,
            content_hash: marker.content_hash,
            proof: marker.proof,
            for_kind: "bundle".to_owned(),
            for_id: marker_id.as_uuid().to_string(),
        };
        let mut entries = Vec::with_capacity(ids.len());
        for (entry_id, for_kind, for_id) in ids {
            let bundle = self.entry_bundle(*entry_id, head).await?;
            entries.push(EvidenceEntryBundle {
                entry: bundle.entry,
                content_hash: bundle.content_hash,
                proof: bundle.proof,
                for_kind: for_kind.clone(),
                for_id: for_id.clone(),
            });
        }
        Ok((head, marker, entries))
    }
    /// A refused gateway request's billing record: one marker entry appended
    /// under `req-<request>:refused`, so a request admission turned away still
    /// lands on the billing page with its terminal status and reason (issue
    /// #233). The marker is a self-balancing 1-minor round-trip on the
    /// adjustments account — like the statement anchor — because a refusal
    /// moves no money but is still a billing fact the ledger proves: the
    /// description is a settlement record of kind `refused`, so the requests
    /// page reads it the same way it reads every other outcome.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`]. A request id already
    /// marked replays the first marker: the append and the read answer the
    /// same id.
    pub async fn record_refusal(
        &self,
        request_id: &str,
        model: &str,
        channel: &str,
        reason: &str,
        actor: Option<&str>,
    ) -> Result<EntryId, WalletError> {
        self.record_refusal_with_guardrails(request_id, model, channel, reason, actor, &[])
            .await
    }
    /// Append a refusal marker retaining the exact evaluated guardrail slots.
    /// This remains best-effort at the HTTP boundary and never authorizes an
    /// execution when retaining evidence fails.
    ///
    /// # Errors
    ///
    /// Refuses invalid or oversized encoded evidence and propagates storage
    /// failures with the same replay behavior as [`Self::record_refusal`].
    #[allow(clippy::too_many_arguments)]
    pub async fn record_refusal_with_guardrails(
        &self,
        request_id: &str,
        model: &str,
        channel: &str,
        reason: &str,
        actor: Option<&str>,
        guardrails: &[crate::SettlementGuardrailOwned],
    ) -> Result<EntryId, WalletError> {
        let key = format!("req-{request_id}:refused");
        let usage = UsageRecord::default();
        // Version 15 commits complete reasons that exceed the inline budget.
        let description = Settlement {
            request: request_id,
            channel,
            model,
            // A refused request served nothing: the asked-for name is what the
            // record reports on both sides.
            requested_model: model,
            served_model: model,
            // A refused request never resolved its route — nothing upstream to name.
            upstream_model: None,
            price_version: 0,
            kind: SettlementKind::Refused,
            usage: &usage,
            lines: &[],
            matched_rule: None,
            discount_percent: None,
            charged: 0,
            freeze: 0,
            upstream_attempts: 0,
            mandate: None,
            subscription: None,
            cache: None,
            guardrail: guardrails
                .iter()
                .map(crate::SettlementGuardrailOwned::borrowed)
                .collect(),
            reason: Some(reason),
            experiment: None,
            // A refused request never priced: no sale terms to commit.
            sale: None,
        }
        .description()?;
        crate::validate_gateway_description(&description)?;
        let mut draft = Entry::<Draft, SCALE>::new(
            entry_id_for(&key),
            idem(&key)?,
            OffsetDateTime::now_utc().date(),
        )
        .with_description(description_of(&description)?);
        if let Some(actor) = actor {
            draft = draft.with_provenance(Provenance::none().with_actor(actor).map_err(invalid)?);
        }
        let minor = Credits::from_minor(1);
        let entry = draft.debit(self.adjustments, minor, currency()).credit(
            self.adjustments,
            minor,
            currency(),
        );
        let receipt = match self.append(entry).await {
            Ok(receipt) => receipt,
            Err(error) => {
                // A raced or repeated marker lands on the same key; the
                // standing entry is the same refusal either way.
                let Some(stored) = self.store.get(entry_id_for(&key)).await? else {
                    return Err(error);
                };
                stored_receipt(&stored)
            }
        };
        Ok(receipt.entry_id)
    }
    /// The current tree head, signed by the operator: what `GET /api/v1/log/head` serves.
    ///
    /// The note attests the head at signing time; the ledger keeps growing underneath, and a
    /// later call signs a later head. Nothing is stored: the signature is recomputed from the
    /// key, so rotating the key rotates every head without a migration.
    pub async fn signed_head(&self, key: &HeadSigningKey) -> Result<SignedHead, WalletError> {
        let origin = origin_for(&self.tenant_id).map_err(invalid)?;
        let head = self.store.head().await?;
        sign_head(key, &origin, head).map_err(invalid)
    }
    /// The head at an earlier log size, signed by the operator: what the statement
    /// anchor serves (issue #221) — the head right after the marker is what proves
    /// the anchor's inclusion.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] when `size` exceeds the log's current size.
    pub async fn signed_head_at(
        &self,
        key: &HeadSigningKey,
        size: u64,
    ) -> Result<SignedHead, WalletError> {
        let origin = origin_for(&self.tenant_id).map_err(invalid)?;
        let head = self.store.head_at(size).await.map_err(invalid)?;
        sign_head(key, &origin, head).map_err(invalid)
    }
    /// A consistency proof from an earlier size to the current head, with the new head signed:
    /// what `GET /api/v1/log/consistency` serves.
    ///
    /// `from` must name a real head: at least 1 — every log extends the empty tree, so a
    /// proof from size 0 would verify against any history at all and is refused — and at most
    /// the current size. The old head is recomputed from the log, not taken from the caller,
    /// so the two heads and the proof always agree with each other.
    pub async fn consistency(
        &self,
        from: u64,
        key: &HeadSigningKey,
    ) -> Result<Consistency, WalletError> {
        if from == 0 {
            return Err(WalletError::InvalidInput(
                "consistency proofs start at size 1: a proof from the empty tree would verify against any history".into(),
            ));
        }
        let head = self.store.head().await?;
        if from > head.size {
            return Err(WalletError::InvalidInput(format!(
                "cannot prove consistency from size {from} against a log of size {}",
                head.size
            )));
        }
        // The proof is built against the head just read, and the response signs that same
        // head: a write landing between the two leaves the answer one entry behind the
        // absolute latest, still a true statement about the head it attests.
        let proof = self
            .store
            .prove_consistency_between(from, head.size)
            .await
            .map_err(invalid)?;
        let old_head = self.store.head_at(from).await.map_err(invalid)?;
        let origin = origin_for(&self.tenant_id).map_err(invalid)?;
        Ok(Consistency {
            signed: sign_head(key, &origin, head).map_err(invalid)?,
            old_head,
            proof,
        })
    }
    /// Number of entries in the log. With isolated ledgers, each tenant counts from zero.
    pub async fn log_size(&self) -> Result<u64, WalletError> {
        Ok(self.store.head().await?.size)
    }
    /// The signed price binding for one settlement entry, or `None` when it is not a
    /// settlement or the price it names is gone.
    ///
    /// Reads the `(channel, model, priceVersion)` the settlement description records —
    /// the same fields the entry's content hash already covers — fetches that row, and
    /// signs it as a catalog checkpoint. Signing is stateless and deterministic over
    /// (key, origin, version, row), so the note a v1 bundle carries is identical
    /// whether it was made at bill time or after the catalog moved on.
    async fn price_link(
        &self,
        stored: &StoredEntry<SCALE>,
        key: &HeadSigningKey,
    ) -> Result<Option<oxsum_verify::PriceLink>, WalletError> {
        let Some(record) = SettlementRecord::parse(stored.entry.description().as_str()) else {
            return Ok(None);
        };
        // The catalog lives in the shared `oxsum` schema on the same pool the
        // tenant ledger runs on — no second handle to thread through.
        let Some(row) = crate::channels::price_row(
            self.store.pool(),
            &record.channel,
            &record.model,
            Some(record.price_version),
        )
        .await?
        else {
            return Ok(None);
        };
        let canonical = canonical_price_row(&record.channel, &row)?;
        let signed = sign_price_head(key, &record.channel, &row, &canonical).map_err(invalid)?;
        Ok(Some(
            oxsum_verify::PriceLink::builder()
                .channel(signed.channel)
                .model(signed.model)
                .version(signed.version)
                .row(canonical)
                .note(signed.note)
                .key_name(signed.key.name)
                .public_key(hex(&signed.key.public_key))
                .key_hash(hex(&signed.key.key_hash))
                .build(),
        ))
    }
    /// The signed sale binding for one settlement entry, or `None` when the
    /// entry is not a settlement, the settlement priced under the wholesale
    /// channel catalog (its description commits no `sale` digest), or the
    /// snapshot the digest names was never recorded.
    ///
    /// Reads the `sale.digest` the settlement description commits — the same
    /// field the entry's content hash already covers — fetches the canonical
    /// snapshot row the digest names, and signs it as a sale-catalog checkpoint
    /// (issue #600). Signing is stateless and deterministic over (key, origin,
    /// version, row), so the note a bundle carries is identical whether it was
    /// made at bill time or after the sale book moved on. The canonical row
    /// binds only the customer-visible terms, so the evidence cannot attest a
    /// supplier rate.
    async fn sale_link(
        &self,
        stored: &StoredEntry<SCALE>,
        key: &HeadSigningKey,
    ) -> Result<Option<oxsum_verify::SaleLink>, WalletError> {
        let Some(record) = SettlementRecord::parse(stored.entry.description().as_str()) else {
            return Ok(None);
        };
        let Some(sale) = record.sale else {
            return Ok(None);
        };
        // The digest names the stored canonical row verbatim — the bytes the
        // checkpoint's root commits to, so re-serving them keeps the proof
        // honest however the sale book has since moved.
        let Some((business, canonical)) = crate::Db::from_pool(self.store.pool().clone())
            .sale_snapshot_by_digest(&sale.digest)
            .await?
        else {
            return Ok(None);
        };
        let snapshot: serde_json::Value = serde_json::from_str(&canonical)
            .map_err(|e| WalletError::Misconfigured(format!("stored sale row: {e}")))?;
        let str_field = |field: &str| {
            snapshot
                .get(field)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        };
        let organization: uuid::Uuid = str_field("organizationId")
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| invalid("stored sale row has no organizationId"))?;
        let model = str_field("model").ok_or_else(|| invalid("stored sale row has no model"))?;
        let version = snapshot
            .get("version")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);
        let signed = sign_sale_checkpoint(key, business, organization, &model, version, &canonical)
            .map_err(invalid)?;
        Ok(Some(
            oxsum_verify::SaleLink::builder()
                .organization_id(organization.to_string())
                .model(model)
                .digest(sale.digest.clone())
                .row(canonical)
                .note(signed.note)
                .key_name(signed.key.name)
                .public_key(hex(&signed.key.public_key))
                .key_hash(hex(&signed.key.key_hash))
                .build(),
        ))
    }
    ///
    /// When `key` is the operator's signing key and the entry is a settlement that
    /// names a price, the bundle also carries a [`PriceLink`](oxsum_verify::PriceLink)
    /// — the signed checkpoint binding the `(channel, model, priceVersion)` the bill
    /// records to the catalog row it priced under (issue #204). `None` key — a
    /// deployment that signs no heads — leaves `price` absent, as does a
    /// non-settlement entry or a settlement whose named row is gone; the verifier
    /// reports the binding absent rather than failed.
    pub async fn receipt_proof(
        &self,
        entry_id: EntryId,
        key: Option<&HeadSigningKey>,
    ) -> Result<Option<ProofBundle>, WalletError> {
        let Some(stored) = self.store.get(entry_id).await? else {
            return Ok(None);
        };
        let index = stored.require_index().map_err(invalid)?;
        let head = self.store.head().await?;
        let proof = self.store.prove_inclusion(index).await?;
        let (price, sale) = match key {
            Some(key) => (
                self.price_link(&stored, key).await?,
                self.sale_link(&stored, key).await?,
            ),
            None => (None, None),
        };
        Ok(Some(ProofBundle::new(
            stored.entry,
            head,
            proof,
            price,
            sale,
        )))
    }
    /// Customer evidence must apply the same attribution scope as record lists.
    pub async fn scoped_receipt_proof(
        &self,
        entry_id: EntryId,
        key: Option<&HeadSigningKey>,
        scope: &crate::CustomerRecordScope,
    ) -> Result<Option<ProofBundle>, WalletError> {
        let Some(stored) = self.store.get(entry_id).await? else {
            return Ok(None);
        };
        if !scope.allows_entry(&stored.entry) {
            return Ok(None);
        }
        self.receipt_proof(entry_id, key).await
    }
}
