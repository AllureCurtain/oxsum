//! The metering surface's orchestration (issue #172, roadmap P8-4).
//!
//! A service credential's call prices a billable event: `hold` freezes the
//! declared usage's itemized cost under an opaque credential/customer-scoped key,
//! `settle` charges the event's actual usage against it — capped at the
//! freeze — `release` lets it go at zero, and a one-shot `settlement` lands
//! hold and settle under keys derived from the one idempotency key. Pricing
//! resolves `(channel, billableCode)` to the channel's latest `mode: event`
//! price version; a hold pins it, and the settle reads the pin from the
//! sweeper's watch row. Declared usage carrying a dimension the price cannot
//! bill is refused outright: the service writes the usage, so it can fix the
//! request — the `unpriced` kind exists for upstream reports, which nobody can.
//!
//! Metering holds ride the same watch table as gateway holds: a hold that
//! never settles is swept to zero like any other, and every write carries the
//! credential as the ledger entry's actor, so the log answers "which service
//! reported this" the way a key hold names its key.

use std::sync::Arc;

use crate::integration_credentials::{IntegrationScope, require_integration};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::Row;
use time::Date;
use uuid::Uuid;

use crate::billing::{BillLine, MeteredSettlement, SettlementKind, hold_description};
use crate::db::Db;
use crate::error::WalletError;
use crate::holds::OpenHold;
use crate::integration_credentials::ActingService;
use crate::operations::{
    METERED_OUTCOME, METERING_SURFACE, NewOperation, OperationSettle, OperationState,
};
use crate::tenants::Tenants;
use crate::usage::{CostBasis, CountsProvenance, UsageRecord, UsageRow};
use crate::wallet::{Wallet, entry_id_for, settlement_key_for};

/// What a metering `hold` or one-shot `settlement` names: the price to resolve
/// and the usage the event is bounded by — or settled at.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeteredEvent {
    /// The channel whose `event`-mode price for the code applies.
    pub channel: String,
    /// The billable code — the price book's model name on that channel.
    pub billable_code: String,
    /// The service's own event label, recorded in settlement evidence.
    /// Usage/watch rows use the scoped hold key.
    /// `None` uses the hold key.
    pub event_id: Option<String>,
    /// The declared usage — a hold's bound, a settlement's actual.
    pub usage: UsageRecord,
}

/// What a `holds` call answers: the key `settle` and `release` name, the frozen
/// bound and the price version pinned to it.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeteredHold {
    pub hold_key: String,
    pub freeze_minor: i64,
    pub price_version: i64,
}

/// What a `settle`, `release` or one-shot `settlements` call answers.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeteredOutcome {
    pub charged_minor: i64,
    pub kind: SettlementKind,
    /// The version the event priced at — the hold's pin on `settle`/`release`,
    /// latest on a one-shot.
    pub price_version: i64,
    /// The settlement ledger entry's id — derivable, but answering it saves the
    /// service a derivation.
    pub settlement_id: Uuid,
}

/// An event id's bound: it lands as the usage row's request id and inside the
/// settlement description, which the ledger caps at 512 characters.
const MAX_EVENT_ID: usize = 128;

/// The credential's name as the settlement description records it: its `name`
/// when it carries one, its id when it does not.
fn service_name(service: &ActingService) -> String {
    service
        .name
        .clone()
        .unwrap_or_else(|| format!("svc-{}", service.credential_id.as_simple()))
}

/// Scoped and bounded even when a caller reuses a key in another customer.
fn hold_key_for(service: &ActingService, customer: Uuid, key: &str) -> Result<String, WalletError> {
    crate::business_staff::validate(key, None)?;
    let digest = Sha256::digest(
        json!((service.credential_id, customer, key))
            .to_string()
            .as_bytes(),
    );
    let digest = digest
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    Ok(format!("metering:{digest}:hold"))
}

#[derive(Serialize, Deserialize)]
struct Admission {
    hold_key: String,
    request_id: String,
    price: crate::Price,
    version: i64,
    freeze: i64,
    discount: Option<i64>,
}
/// Persist original terms before writing the hold. Permissions are read in the
/// same transaction as the immutable authorization, including on retries.
async fn authorize_start(
    db: &Db,
    service: &ActingService,
    customer: Uuid,
    key: &str,
    event: &MeteredEvent,
    once: bool,
) -> Result<Admission, WalletError> {
    let hold_key = hold_key_for(service, customer, key)?;
    let request = json!({"operation":if once {"settlements"} else {"holds"},"event":event});
    let mut tx = db.pool().begin().await?;
    require_integration(
        &mut tx,
        service,
        IntegrationScope::MeteringWrite,
        Some(customer),
    )
    .await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,638))")
        .bind(&hold_key)
        .execute(&mut *tx)
        .await?;
    if let Some(row) =
        sqlx::query("SELECT request,admission FROM oxsum.metering_authorizations WHERE hold_key=$1")
            .bind(&hold_key)
            .fetch_optional(&mut *tx)
            .await?
    {
        if row.try_get::<Value, _>("request")? != request {
            return Err(WalletError::Conflict(
                "idempotencyKey was used for different metering terms".into(),
            ));
        }
        return serde_json::from_value(row.try_get("admission")?)
            .map_err(|_| WalletError::Misconfigured("unreadable metering authorization".into()));
    }
    check_event(event)?;
    let price = crate::channels::event_price(
        &mut tx,
        service.business_id,
        &event.channel,
        &event.billable_code,
        None,
    )
    .await?
    .ok_or_else(|| {
        WalletError::InvalidInput("no owned event-mode price for channel and billableCode".into())
    })?;
    check_usage(&event.usage, &price.price, &event.billable_code)?;
    let discount:Option<i32>=sqlx::query_scalar("SELECT max(percent) FROM oxsum.pricing_discounts WHERE (organization_id IS NULL OR organization_id=$1) AND (model IS NULL OR model=$2) AND valid_from<=now() AND (valid_until IS NULL OR valid_until>now())")
        .bind(customer).bind(&event.billable_code).fetch_one(&mut *tx).await?;
    let discount = discount.map(i64::from);
    let itemized = price.price.itemize(&event.usage, true)?;
    let freeze = if once {
        match discount {
            Some(p) => itemized.discounted_minor(p)?,
            None => itemized.total_minor()?,
        }
    } else {
        itemized.total_minor()?
    };
    if freeze <= 0 {
        return Err(WalletError::InvalidInput(
            "the declared usage prices to zero".into(),
        ));
    }
    let admission = Admission {
        request_id: event.event_id.clone().unwrap_or_else(|| hold_key.clone()),
        hold_key,
        price: price.price,
        version: price.version,
        freeze,
        discount,
    };
    sqlx::query("INSERT INTO oxsum.metering_authorizations (hold_key,business_id,credential_id,organization_id,idempotency_key,request,admission) VALUES ($1,$2,$3,$4,$5,$6,$7)")
        .bind(&admission.hold_key).bind(service.business_id).bind(service.credential_id).bind(customer).bind(key).bind(request).bind(json!(admission)).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(admission)
}
/// Bind the finalization body before settlement. A committed retry with changed
/// usage is a conflict, never a read of an unrelated result.
async fn authorize_finish(
    db: &Db,
    service: &ActingService,
    customer: Uuid,
    hold_key: &str,
    request: Value,
) -> Result<Admission, WalletError> {
    let mut tx = db.pool().begin().await?;
    require_integration(
        &mut tx,
        service,
        IntegrationScope::MeteringWrite,
        Some(customer),
    )
    .await?;
    let row=sqlx::query("SELECT request,admission,finalization FROM oxsum.metering_authorizations WHERE hold_key=$1 AND business_id=$2 AND credential_id=$3 AND organization_id=$4 FOR UPDATE")
        .bind(hold_key).bind(service.business_id).bind(service.credential_id).bind(customer).fetch_optional(&mut *tx).await?.ok_or_else(||WalletError::NotFound("the hold is not known".into()))?;
    let original: Value = row.try_get("request")?;
    if original["operation"] == "settlements" && request["operation"] != "settlements" {
        return Err(WalletError::Conflict(
            "hold belongs to a one-shot settlement".into(),
        ));
    }
    let admission: Admission = serde_json::from_value(row.try_get("admission")?)
        .map_err(|_| WalletError::Misconfigured("unreadable metering authorization".into()))?;
    if let Some(usage) = request.get("usage") {
        let usage: UsageRecord = serde_json::from_value(usage.clone())
            .map_err(|_| WalletError::InvalidInput("invalid usage".into()))?;
        check_usage(
            &usage,
            &admission.price,
            original["event"]["billableCode"].as_str().unwrap_or(""),
        )?;
    }
    if let Some(id) = request.get("eventId").and_then(Value::as_str)
        && (id.is_empty() || id.len() > MAX_EVENT_ID)
    {
        return Err(WalletError::InvalidInput(
            "eventId must contain 1 to 128 bytes".into(),
        ));
    }
    if let Some(previous) = row.try_get::<Option<Value>, _>("finalization")? {
        if previous != request {
            return Err(WalletError::Conflict(
                "hold was finalized with different metering terms".into(),
            ));
        }
    } else {
        sqlx::query("UPDATE oxsum.metering_authorizations SET finalization=$2 WHERE hold_key=$1")
            .bind(hold_key)
            .bind(request)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(admission)
}

/// The shape checks every metering event passes before a price is looked up:
/// the code is a model name and the event id is a request id, so both keep
/// those bounds.
fn check_event(event: &MeteredEvent) -> Result<(), WalletError> {
    if event.billable_code.trim().is_empty() || event.billable_code.len() > 200 {
        return Err(WalletError::InvalidInput(
            "billableCode must be 1 to 200 characters".into(),
        ));
    }
    if let Some(id) = &event.event_id
        && (id.is_empty() || id.len() > MAX_EVENT_ID)
    {
        return Err(WalletError::InvalidInput(format!(
            "eventId must be 1 to {MAX_EVENT_ID} characters"
        )));
    }
    Ok(())
}

/// The checks every declared usage passes before money moves: the record's own
/// invariants, an `eventType` — an external event names its kind, that is the
/// whole point of the surface — and that the price can bill every dimension the
/// declaration carries.
fn check_usage(usage: &UsageRecord, price: &crate::Price, code: &str) -> Result<(), WalletError> {
    usage.validate()?;
    if usage.event_type.is_none() {
        return Err(WalletError::InvalidInput(
            "usage.eventType names the event and is required".into(),
        ));
    }
    let unpriced = price.unpriced_dimensions(usage);
    if !unpriced.is_empty() {
        return Err(WalletError::InvalidInput(format!(
            "the {code:?} price cannot bill: {}",
            unpriced.join(", ")
        )));
    }
    Ok(())
}

/// The organization a metering call names, and its wallet.
async fn metered_wallet(
    db: &Db,
    tenants: &Tenants,
    organization_id: Uuid,
) -> Result<(String, Arc<Wallet>), WalletError> {
    let organization = db.organization_by_id(organization_id).await?;
    let wallet = tenants.get(&organization.tenant_id).await?;
    Ok((organization.tenant_id, wallet))
}

/// Writes the settled event's usage row and clears the hold's watch — the same
/// pair a gateway turn writes; a failed write is drift for the reconciler, not
/// a reason to retry the charge. The wallet is in hand to read the post-settle
/// balance `balance.low` thresholds judge against.
async fn record_metered_usage(db: &Db, wallet: &Wallet, row: &UsageRow, hold_key: &str) {
    let available = wallet.available().await.ok();
    if let Err(error) = db.record_usage_balanced(row, available).await {
        tracing::error!(%error, hold_key, "recording the metered event's usage row failed");
    }
    if let Err(error) = db.clear_open_hold(hold_key).await {
        tracing::error!(%error, hold_key, "clearing the metered hold's watch row failed");
    }
}

/// The outcome a committed settlement for `hold_key` recorded — the replay
/// answer every metering endpoint shares: the entry id derives from the hold's
/// key, so the read is one probe.
async fn committed_outcome(
    wallet: &Wallet,
    hold_key: &str,
) -> Result<Option<MeteredOutcome>, WalletError> {
    Ok(wallet
        .settled_record(hold_key)
        .await?
        .map(|record| MeteredOutcome {
            charged_minor: record.charged,
            kind: record.kind,
            price_version: record.price_version,
            settlement_id: *entry_id_for(&settlement_key_for(hold_key)).as_uuid(),
        }))
}

/// The outcome a settle conflict answers: a committed settlement replays its
/// record; a conflict without one means the hold is spoken for by a write this
/// build cannot read, so the watch row is drift to clear and the answer is a
/// plain conflict.
async fn settled_outcome(
    db: &Db,
    wallet: &Wallet,
    hold_key: &str,
    expected: &[SettlementKind],
) -> Result<MeteredOutcome, WalletError> {
    if let Some(outcome) = committed_outcome(wallet, hold_key).await? {
        return if expected.contains(&outcome.kind) {
            Ok(outcome)
        } else {
            Err(WalletError::Conflict("hold already settled".into()))
        };
    }
    let _ = db.clear_open_hold(hold_key).await;
    Err(WalletError::Conflict("hold already settled".into()))
}

/// What the watch-row probe a `settle` or `release` makes found.
enum Watched {
    /// The live hold the call works from.
    Live(Box<OpenHold>),
    /// The committed settlement this call's own retry replays.
    Committed(MeteredOutcome),
}

/// The watch row a `settle` or `release` works from. Its absence means the hold
/// is not outstanding: a committed settlement answers its stored outcome, and
/// anything else was never this organization's hold.
async fn watched(
    db: &Db,
    wallet: &Wallet,
    hold_key: &str,
    tenant_id: &str,
) -> Result<Watched, WalletError> {
    if let Some(watch) = db
        .open_hold(hold_key)
        .await?
        .filter(|hold| hold.tenant_id == tenant_id)
    {
        return Ok(Watched::Live(Box::new(watch)));
    }
    match committed_outcome(wallet, hold_key).await? {
        Some(outcome) => Ok(Watched::Committed(outcome)),
        None => Err(WalletError::NotFound("the hold is not outstanding".into())),
    }
}

/// The lease owner a metered event's operation binds and settles under (issue
/// #604): fixed rather than an instance id, like the sweeper's — a metered
/// hold is short-lived and never streamed, so its lease is a claim the settle
/// makes, not a heartbeat a worker keeps.
const METERING_OWNER: &str = "metering";

impl Db {
    /// Writes the metered event's operation row and binds it to the hold the
    /// intake just took (issue #604): `surface` `metering`, `fingerprint` the
    /// `hold_key` so the settle names the same row, `hold_key` bound at once
    /// because the hold already exists. `ON CONFLICT` on the fingerprint makes
    /// a replayed `holds` idempotent — the existing row comes back, and its
    /// `bind_hold` is a `created`-only CAS the replay loses cleanly. A failed
    /// write is logged, never a failed hold: the ledger entry is the bill.
    async fn note_metered_operation(
        &self,
        organization_id: Uuid,
        hold_key: &str,
        billable_code: &str,
        channel: &str,
        freeze: i64,
        version: i64,
    ) {
        let inserted = self
            .insert_operation(&NewOperation {
                operation_id: Uuid::new_v4(),
                organization_id,
                key_id: None,
                surface: METERING_SURFACE.to_owned(),
                fingerprint: hold_key.to_owned(),
                bound_minor: freeze,
                model: billable_code.to_owned(),
                channel: Some(channel.to_owned()),
                customer_id: None,
                terms_version: Some(version),
            })
            .await;
        let operation = match inserted {
            Ok((operation, _fresh)) => operation,
            Err(error) => {
                tracing::error!(%error, hold_key,
                    "writing the metered event's operation row failed");
                return;
            }
        };
        if let Err(error) = self
            .bind_hold(operation.operation_id, hold_key, METERING_OWNER)
            .await
        {
            tracing::error!(%error, hold_key,
                "binding the metered operation's hold failed");
        }
    }

    /// Closes the metered event's operation under the `metered` outcome (issue
    /// #604): the settle names the row the hold wrote by its `hold_key`
    /// fingerprint, so a cost-allocation read over `request_operations` never
    /// confuses a privileged internal event with customer-billable usage. A
    /// missing or raced row is logged, never the settle's failure — the ledger
    /// entry already decided the event's money.
    async fn settle_metered_operation(
        &self,
        organization_id: Uuid,
        hold_key: &str,
        state: OperationState,
        charged: i64,
    ) {
        let operation = match self
            .operation_by_fingerprint(organization_id, hold_key)
            .await
        {
            Ok(Some(operation)) => operation,
            Ok(None) => {
                tracing::warn!(hold_key, "no metered operation row to settle");
                return;
            }
            Err(error) => {
                tracing::error!(%error, hold_key, "reading the metered operation failed");
                return;
            }
        };
        match self
            .settle_operation(
                operation.operation_id,
                METERING_OWNER,
                state,
                charged,
                METERED_OUTCOME,
            )
            .await
        {
            Ok(OperationSettle::Settled) => {}
            Ok(OperationSettle::AlreadySettled) => {
                tracing::info!(hold_key, "the metered operation was already terminal");
            }
            Ok(OperationSettle::LeaseLost) => {
                tracing::warn!(hold_key, "the metered operation's lease was lost");
            }
            Err(error) => {
                tracing::error!(%error, hold_key, "settling the metered operation failed");
            }
        }
    }

    /// Freezes `event`'s declared usage — the bound — as a hold in the named
    /// organization's wallet, attributed to the reporting credential.
    ///
    /// The watch row goes in before the hold is taken: a row without a hold
    /// heals itself, a hold without a row is invisible to the sweeper.
    ///
    /// # Errors
    ///
    /// [`WalletError::NotFound`] for an unknown organization;
    /// [`WalletError::InvalidInput`] for an unknown channel/code, usage that is
    /// malformed, unpriced by the resolved price or priced at zero;
    /// [`WalletError::Forbidden`] when the organization is suspended;
    /// [`WalletError::InsufficientFunds`] when the balance cannot cover the
    /// bound.
    pub async fn metering_hold(
        &self,
        tenants: &Tenants,
        service: &ActingService,
        organization_id: Uuid,
        idempotency_key: &str,
        event: &MeteredEvent,
        on: Date,
    ) -> Result<MeteredHold, WalletError> {
        let admission = authorize_start(
            self,
            service,
            organization_id,
            idempotency_key,
            event,
            false,
        )
        .await?;
        let (tenant_id, wallet) = metered_wallet(self, tenants, organization_id).await?;
        let freeze = admission.freeze;
        let hold_key = admission.hold_key;
        let request_id = admission.request_id;
        if committed_outcome(&wallet, &hold_key).await?.is_some() {
            return Ok(MeteredHold {
                hold_key,
                freeze_minor: freeze,
                price_version: admission.version,
            });
        }
        self.note_open_hold(&OpenHold {
            hold_key: hold_key.clone(),
            tenant_id,
            request_id: hold_key.clone(),
            model: event.billable_code.clone(),
            channel: event.channel.clone(),
            price_version: admission.version,
            input_price: admission.price.input_price_per_million,
            output_price: admission.price.output_price_per_million,
            freeze_minor: freeze,
            key_id: None,
            mandate_id: None,
            end_user: event.usage.end_user.clone(),
            service_tier: event.usage.service_tier.clone(),
            tags: event.usage.tags.clone(),
            sweep_attempts: 0,
            last_error: None,
            dead_at: None,
        })
        .await?;
        let description = hold_description(&request_id, &event.billable_code, freeze)?;
        if let Err(error) = wallet
            .hold_for_service(&service.credential_id, &hold_key, &description, freeze, on)
            .await
        {
            // The hold was refused, so there is nothing for the sweeper to
            // watch — except on a conflict, where the row belongs to the hold
            // that won the key and clearing it would unwatch that one.
            if !matches!(error, WalletError::Conflict(_))
                && let Err(clear) = self.clear_open_hold(&hold_key).await
            {
                tracing::error!(%clear, "clearing a refused metering hold's watch row failed");
            }
            return Err(error);
        }
        // The metered event's operation row (issue #604): the privileged
        // service-credential intake produces a durable operation like every
        // customer surface, marked `metering` and keyed by `hold_key` as its
        // fingerprint so the settle names the same row — and so a replayed
        // `holds` call under the same idempotency key re-binds rather than
        // writing a second operation. `key_id` stays NULL: the caller is a
        // service credential, not a consumption key. Best-effort like the
        // usage record — a failed write is a thinner recovery trail, never a
        // failed hold.
        self.note_metered_operation(
            organization_id,
            &hold_key,
            &event.billable_code,
            &event.channel,
            freeze,
            admission.version,
        )
        .await;
        // The version the answer names is the hold's pin, not the version just
        // resolved: on a replay the price book may have moved on. The watch row
        // carries it while the hold is open; once it settled, the committed
        // record does.
        let pinned = match self.open_hold(&hold_key).await? {
            Some(watch) => watch.price_version,
            None => committed_outcome(&wallet, &hold_key)
                .await?
                .map(|outcome| outcome.price_version)
                .unwrap_or(admission.version),
        };
        Ok(MeteredHold {
            hold_key,
            freeze_minor: freeze,
            price_version: pinned,
        })
    }

    /// Settles a metering hold against the event's declared actual usage, at
    /// the version the hold pinned — `min(itemized, freeze)`, `usage` or
    /// `capped`.
    ///
    /// # Errors
    ///
    /// [`WalletError::NotFound`] for an organization or hold that is not
    /// outstanding; [`WalletError::InvalidInput`] for malformed or unpriceable
    /// usage.
    #[allow(clippy::too_many_arguments)]
    pub async fn metering_settle(
        &self,
        tenants: &Tenants,
        service: &ActingService,
        organization_id: Uuid,
        hold_key: &str,
        event_id: Option<String>,
        usage: &UsageRecord,
        on: Date,
    ) -> Result<MeteredOutcome, WalletError> {
        let admission = authorize_finish(
            self,
            service,
            organization_id,
            hold_key,
            json!({"operation":"settle","eventId":event_id,"usage":usage}),
        )
        .await?;
        let (tenant_id, wallet) = metered_wallet(self, tenants, organization_id).await?;
        let watch = match watched(self, &wallet, hold_key, &tenant_id).await? {
            Watched::Live(watch) => watch,
            // A committed settlement is this call's own retry when it settled
            // the way a settle does; a `released`/`swept` record is another
            // operation's, and settling over it is a conflict.
            Watched::Committed(outcome)
                if matches!(outcome.kind, SettlementKind::Usage | SettlementKind::Capped) =>
            {
                return Ok(outcome);
            }
            Watched::Committed(_) => {
                return Err(WalletError::Conflict("hold already settled".into()));
            }
        };
        if let Some(id) = &event_id
            && (id.is_empty() || id.len() > MAX_EVENT_ID)
        {
            return Err(WalletError::InvalidInput(format!(
                "eventId must be 1 to {MAX_EVENT_ID} characters"
            )));
        }
        let discount_percent = admission.discount;
        let itemized = admission.price.itemize(usage, true)?;
        let cost = match discount_percent {
            Some(percent) => itemized.discounted_minor(percent)?,
            None => itemized.total_minor()?,
        };
        let (kind, charged) = if cost > watch.freeze_minor {
            (SettlementKind::Capped, watch.freeze_minor)
        } else {
            (SettlementKind::Usage, cost)
        };
        let request_id = event_id.unwrap_or_else(|| admission.request_id.clone());
        let description = MeteredSettlement {
            request: &request_id,
            service: &service_name(service),
            channel: &watch.channel,
            model: &watch.model,
            price_version: watch.price_version,
            kind,
            usage,
            lines: &itemized.lines,
            matched_rule: itemized.matched_rule.as_ref(),
            discount_percent,
            charged,
            freeze: watch.freeze_minor,
        }
        .description()?;
        match wallet.settle(hold_key, &description, charged, on).await {
            Ok(_) => {}
            Err(WalletError::Conflict(_)) | Err(WalletError::HoldNotFound(_)) => {
                return settled_outcome(
                    self,
                    &wallet,
                    hold_key,
                    &[SettlementKind::Usage, SettlementKind::Capped],
                )
                .await;
            }
            Err(error) => return Err(error),
        }
        let upstream_cost = admission
            .price
            .upstream_cost(usage, true)
            .unwrap_or_else(|error| {
                tracing::error!(%error, hold_key,
                "pricing the metered event's upstream cost failed; the row records untracked");
                None
            });
        record_metered_usage(
            self,
            &wallet,
            &UsageRow {
                request_id: hold_key.to_owned(),
                tenant_id,
                key_id: None,
                mandate_id: None,
                model: watch.model.clone(),
                channel: watch.channel.clone(),
                price_version: watch.price_version,
                kind,
                entry_id: *entry_id_for(&settlement_key_for(hold_key)).as_uuid(),
                status: "settled".to_owned(),
                failure_reason: None,
                usage: usage.clone(),
                counts: CountsProvenance::Provider {
                    input: usage.input_tokens,
                    output: usage.output_tokens,
                },
                charged_minor: charged,
                freeze_minor: watch.freeze_minor,
                upstream_cost_minor: upstream_cost,
                // The reporting service attested real consumption (issue
                // #604): `real_attempt` when the price priced it, `missing`
                // when it carried no `upstream` block.
                cost_basis: if upstream_cost.is_some() {
                    CostBasis::RealAttempt
                } else {
                    CostBasis::Missing
                },
                upstream_attempts: 0,
            },
            hold_key,
        )
        .await;
        self.settle_metered_operation(
            organization_id,
            hold_key,
            OperationState::Succeeded,
            charged,
        )
        .await;
        Ok(MeteredOutcome {
            charged_minor: charged,
            kind,
            price_version: watch.price_version,
            settlement_id: *entry_id_for(&settlement_key_for(hold_key)).as_uuid(),
        })
    }

    /// Releases a metering hold the event will never settle: a settlement at
    /// zero with kind `released` — the service's own answer, distinct from the
    /// sweeper's `swept`.
    ///
    /// # Errors
    ///
    /// [`WalletError::NotFound`] for an organization or hold that is not
    /// outstanding.
    pub async fn metering_release(
        &self,
        tenants: &Tenants,
        service: &ActingService,
        organization_id: Uuid,
        hold_key: &str,
        on: Date,
    ) -> Result<MeteredOutcome, WalletError> {
        let admission = authorize_finish(
            self,
            service,
            organization_id,
            hold_key,
            json!({"operation":"release"}),
        )
        .await?;
        let (tenant_id, wallet) = metered_wallet(self, tenants, organization_id).await?;
        let watch = match watched(self, &wallet, hold_key, &tenant_id).await? {
            Watched::Live(watch) => watch,
            // Only a `released` record is this call's own replay; a hold that
            // already charged is settled, and settling it again is a conflict.
            Watched::Committed(outcome) if outcome.kind == SettlementKind::Released => {
                return Ok(outcome);
            }
            Watched::Committed(_) => {
                return Err(WalletError::Conflict("hold already settled".into()));
            }
        };
        let usage = UsageRecord::default();
        // Zero units at the pinned rates, like a swept record: the lines name
        // the price the hold carried even though nothing billed.
        let lines = [
            BillLine {
                item: "input".into(),
                units: 0,
                price_per_m: watch.input_price,
            },
            BillLine {
                item: "output".into(),
                units: 0,
                price_per_m: watch.output_price,
            },
        ];
        let description = MeteredSettlement {
            request: &admission.request_id,
            service: &service_name(service),
            channel: &watch.channel,
            model: &watch.model,
            price_version: watch.price_version,
            kind: SettlementKind::Released,
            usage: &usage,
            lines: &lines,
            matched_rule: None,
            discount_percent: None,
            charged: 0,
            freeze: watch.freeze_minor,
        }
        .description()?;
        match wallet.settle(hold_key, &description, 0, on).await {
            Ok(_) => {}
            Err(WalletError::Conflict(_)) | Err(WalletError::HoldNotFound(_)) => {
                return settled_outcome(self, &wallet, hold_key, &[SettlementKind::Released]).await;
            }
            Err(error) => return Err(error),
        }
        record_metered_usage(
            self,
            &wallet,
            &UsageRow {
                request_id: hold_key.to_owned(),
                tenant_id,
                key_id: None,
                mandate_id: None,
                model: watch.model.clone(),
                channel: watch.channel.clone(),
                price_version: watch.price_version,
                kind: SettlementKind::Released,
                entry_id: *entry_id_for(&settlement_key_for(hold_key)).as_uuid(),
                status: "settled".to_owned(),
                failure_reason: None,
                usage: usage.clone(),
                counts: CountsProvenance::Provider {
                    input: usage.input_tokens,
                    output: usage.output_tokens,
                },
                charged_minor: 0,
                freeze_minor: watch.freeze_minor,
                // A released hold ran no billable work: the platform's cost is
                // untracked, not a fabricated zero (issue #604).
                upstream_cost_minor: None,
                cost_basis: CostBasis::Missing,
                upstream_attempts: 0,
            },
            hold_key,
        )
        .await;
        // A released hold ends the operation without serving: `failed`, the
        // same terminal a released customer hold takes, so the metering row
        // reads like a customer turn that billed nothing (issue #604).
        self.settle_metered_operation(organization_id, hold_key, OperationState::Failed, 0)
            .await;
        Ok(MeteredOutcome {
            charged_minor: 0,
            kind: SettlementKind::Released,
            price_version: watch.price_version,
            settlement_id: *entry_id_for(&settlement_key_for(hold_key)).as_uuid(),
        })
    }

    /// Bills a completed event in one call: the declared usage itemized at the
    /// code's latest `event` price, landed as a hold settled in the same
    /// answer. A replay of the idempotency key answers the stored settlement —
    /// the hold replays under the derived key and the settle's conflict reads
    /// the committed record back.
    ///
    /// # Errors
    ///
    /// As [`metering_hold`](Self::metering_hold); a hold that cannot cover the
    /// charge is [`WalletError::InsufficientFunds`].
    pub async fn metering_settle_once(
        &self,
        tenants: &Tenants,
        service: &ActingService,
        organization_id: Uuid,
        idempotency_key: &str,
        event: &MeteredEvent,
        on: Date,
    ) -> Result<MeteredOutcome, WalletError> {
        let admission =
            authorize_start(self, service, organization_id, idempotency_key, event, true).await?;
        authorize_finish(
            self,
            service,
            organization_id,
            &admission.hold_key,
            json!({"operation":"settlements","event":event}),
        )
        .await?;
        let (tenant_id, wallet) = metered_wallet(self, tenants, organization_id).await?;
        if let Some(outcome) = committed_outcome(&wallet, &admission.hold_key).await? {
            return if outcome.kind == SettlementKind::Usage {
                Ok(outcome)
            } else {
                Err(WalletError::Conflict("hold already settled".into()))
            };
        }
        let discount_percent = admission.discount;
        let charged = admission.freeze;
        let itemized = admission.price.itemize(&event.usage, true)?;
        let hold_key = admission.hold_key;
        let request_id = admission.request_id;
        // A one-shot hold is watched like a held one: a process dying between
        // the hold and the settle would otherwise leave the freeze pinned
        // forever — the sweeper releases it to zero like any orphaned hold.
        self.note_open_hold(&OpenHold {
            hold_key: hold_key.clone(),
            tenant_id: tenant_id.clone(),
            request_id: hold_key.clone(),
            model: event.billable_code.clone(),
            channel: event.channel.clone(),
            price_version: admission.version,
            input_price: admission.price.input_price_per_million,
            output_price: admission.price.output_price_per_million,
            freeze_minor: charged,
            key_id: None,
            mandate_id: None,
            end_user: event.usage.end_user.clone(),
            service_tier: event.usage.service_tier.clone(),
            tags: event.usage.tags.clone(),
            sweep_attempts: 0,
            last_error: None,
            dead_at: None,
        })
        .await?;
        let hold_description = hold_description(&request_id, &event.billable_code, charged)?;
        if let Err(error) = wallet
            .hold_for_service(
                &service.credential_id,
                &hold_key,
                &hold_description,
                charged,
                on,
            )
            .await
        {
            // The hold was refused, so there is nothing for the sweeper to
            // watch — except on a conflict, where the row belongs to the hold
            // that won the key and clearing it would unwatch that one.
            if !matches!(error, WalletError::Conflict(_))
                && let Err(clear) = self.clear_open_hold(&hold_key).await
            {
                tracing::error!(%clear, "clearing a refused metering hold's watch row failed");
            }
            return Err(error);
        }
        // The one-shot event's operation row (issue #604): written between the
        // hold and the settle, bound under the same `metered` marking a held
        // event's carries, so the single-call path produces the same durable
        // parent the two-call path does.
        self.note_metered_operation(
            organization_id,
            &hold_key,
            &event.billable_code,
            &event.channel,
            charged,
            admission.version,
        )
        .await;
        let description = MeteredSettlement {
            request: &request_id,
            service: &service_name(service),
            channel: &event.channel,
            model: &event.billable_code,
            price_version: admission.version,
            kind: SettlementKind::Usage,
            usage: &event.usage,
            lines: &itemized.lines,
            matched_rule: itemized.matched_rule.as_ref(),
            discount_percent,
            charged,
            freeze: charged,
        }
        .description()?;
        match wallet.settle(&hold_key, &description, charged, on).await {
            Ok(_) => {}
            Err(WalletError::Conflict(_)) | Err(WalletError::HoldNotFound(_)) => {
                return settled_outcome(self, &wallet, &hold_key, &[SettlementKind::Usage]).await;
            }
            Err(error) => return Err(error),
        }
        let upstream_cost =
            admission
                .price
                .upstream_cost(&event.usage, true)
                .unwrap_or_else(|error| {
                    tracing::error!(%error, hold_key,
                        "pricing the metered event's upstream cost failed; the row records untracked");
                    None
                });
        record_metered_usage(
            self,
            &wallet,
            &UsageRow {
                request_id: hold_key.to_owned(),
                tenant_id,
                key_id: None,
                mandate_id: None,
                model: event.billable_code.clone(),
                channel: event.channel.clone(),
                price_version: admission.version,
                kind: SettlementKind::Usage,
                entry_id: *entry_id_for(&settlement_key_for(&hold_key)).as_uuid(),
                status: "settled".to_owned(),
                failure_reason: None,
                usage: event.usage.clone(),
                counts: CountsProvenance::Provider {
                    input: event.usage.input_tokens,
                    output: event.usage.output_tokens,
                },
                charged_minor: charged,
                freeze_minor: charged,
                upstream_cost_minor: upstream_cost,
                // The reporting service attested real consumption (issue
                // #604): `real_attempt` when the price priced it, `missing`
                // when it carried no `upstream` block.
                cost_basis: if upstream_cost.is_some() {
                    CostBasis::RealAttempt
                } else {
                    CostBasis::Missing
                },
                upstream_attempts: 0,
            },
            &hold_key,
        )
        .await;
        self.settle_metered_operation(
            organization_id,
            &hold_key,
            OperationState::Succeeded,
            charged,
        )
        .await;
        Ok(MeteredOutcome {
            charged_minor: charged,
            kind: SettlementKind::Usage,
            price_version: admission.version,
            settlement_id: *entry_id_for(&settlement_key_for(&hold_key)).as_uuid(),
        })
    }
}
