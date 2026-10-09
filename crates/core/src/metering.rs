//! The metering surface's orchestration (issue #172, roadmap P8-4).
//!
//! A service credential's call prices a billable event: `hold` freezes the
//! declared usage's itemized cost under a `metering:<key>:hold` ledger key,
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

use time::Date;
use uuid::Uuid;

use crate::billing::{MeteredSettlement, SettlementKind, hold_description};
use crate::channels::event_price;
use crate::db::Db;
use crate::error::WalletError;
use crate::holds::OpenHold;
use crate::service_credentials::ActingService;
use crate::tenants::Tenants;
use crate::usage::UsageRecord;
use crate::wallet::{entry_id_for, settlement_key_for};

/// What a metering `hold` or one-shot `settlement` names: the price to resolve
/// and the usage the event is bounded by — or settled at.
#[derive(Debug, Clone)]
pub struct MeteredEvent {
    /// The channel whose `event`-mode price for the code applies.
    pub channel: String,
    /// The billable code — the price book's model name on that channel.
    pub billable_code: String,
    /// The service's own identifier for the event, recorded as the usage row's
    /// request id. `None` uses the hold key.
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

/// The ledger key a metering hold writes under: `metering:` keeps it out of
/// the organization-side key space — an organization's own hold can never
/// collide with an event's freeze. The engine bounds a key at 128 bytes, so the
/// readable derivation bounds the caller's own key at what the affordance
/// leaves.
fn hold_key_for(idempotency_key: &str) -> Result<String, WalletError> {
    let key = format!("metering:{idempotency_key}:hold");
    if idempotency_key.is_empty() || key.len() > doubleentry::entry::MAX_IDEMPOTENCY_KEY_LEN {
        return Err(WalletError::InvalidInput(format!(
            "idempotencyKey must be 1 to {} characters: the hold key derives as \
             `metering:<key>:hold`, inside the ledger's {}-byte bound",
            doubleentry::entry::MAX_IDEMPOTENCY_KEY_LEN - "metering::hold".len(),
            doubleentry::entry::MAX_IDEMPOTENCY_KEY_LEN,
        )));
    }
    Ok(key)
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

/// The `(channel, billableCode)` resolution every metering call shares: the
/// channel's `event`-mode price for the code, `pinned` when a hold named a
/// version.
async fn resolve_event_price(
    db: &Db,
    channel: &str,
    code: &str,
    pinned: Option<i64>,
) -> Result<crate::channels::ModelPrice, WalletError> {
    event_price(db, channel, code, pinned)
        .await?
        .ok_or_else(|| {
            WalletError::InvalidInput(format!(
                "no event-mode price for billableCode {code:?} on channel {channel:?}"
            ))
        })
}

/// The organization a metering call names, and its wallet.
async fn metered_wallet(
    db: &Db,
    tenants: &Tenants,
    organization_id: Uuid,
) -> Result<(String, std::sync::Arc<crate::wallet::Wallet>), WalletError> {
    let organization = db.organization_by_id(organization_id).await?;
    let wallet = tenants.get(&organization.tenant_id).await?;
    Ok((organization.tenant_id, wallet))
}

/// Writes the settled event's usage row and clears the hold's watch — the same
/// pair a gateway turn writes; a failed write is drift for the reconciler, not
/// a reason to retry the charge.
async fn record_metered_usage(db: &Db, row: &crate::usage::UsageRow, hold_key: &str) {
    if let Err(error) = db.record_usage(row).await {
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
    wallet: &crate::wallet::Wallet,
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
    wallet: &crate::wallet::Wallet,
    hold_key: &str,
) -> Result<MeteredOutcome, WalletError> {
    if let Some(outcome) = committed_outcome(wallet, hold_key).await? {
        return Ok(outcome);
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
    wallet: &crate::wallet::Wallet,
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

impl Db {
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
        let (tenant_id, wallet) = metered_wallet(self, tenants, organization_id).await?;
        check_event(event)?;
        let price = resolve_event_price(self, &event.channel, &event.billable_code, None).await?;
        check_usage(&event.usage, &price.price, &event.billable_code)?;
        let freeze = price.price.itemize(&event.usage, true)?.total_minor()?;
        if freeze <= 0 {
            return Err(WalletError::InvalidInput(
                "the declared usage prices to zero".into(),
            ));
        }
        let hold_key = hold_key_for(idempotency_key)?;
        let request_id = event.event_id.clone().unwrap_or_else(|| hold_key.clone());
        self.note_open_hold(&OpenHold {
            hold_key: hold_key.clone(),
            tenant_id,
            request_id: request_id.clone(),
            model: event.billable_code.clone(),
            channel: event.channel.clone(),
            price_version: price.version,
            input_price: price.price.input_price_per_million,
            output_price: price.price.output_price_per_million,
            freeze_minor: freeze,
            key_id: None,
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
        // The version the answer names is the hold's pin, not the version just
        // resolved: on a replay the price book may have moved on. The watch row
        // carries it while the hold is open; once it settled, the committed
        // record does.
        let pinned = match self.open_hold(&hold_key).await? {
            Some(watch) => watch.price_version,
            None => committed_outcome(&wallet, &hold_key)
                .await?
                .map(|outcome| outcome.price_version)
                .unwrap_or(price.version),
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
        let price = resolve_event_price(
            self,
            &watch.channel,
            &watch.model,
            Some(watch.price_version),
        )
        .await?;
        check_usage(usage, &price.price, &watch.model)?;
        let discount_percent = self
            .discount_percent(organization_id, &watch.model)
            .await?
            .map(i64::from);
        let itemized = price.price.itemize(usage, true)?;
        let cost = match discount_percent {
            Some(percent) => itemized.discounted_minor(percent)?,
            None => itemized.total_minor()?,
        };
        let (kind, charged) = if cost > watch.freeze_minor {
            (SettlementKind::Capped, watch.freeze_minor)
        } else {
            (SettlementKind::Usage, cost)
        };
        let request_id = event_id.unwrap_or_else(|| watch.request_id.clone());
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
                return settled_outcome(self, &wallet, hold_key).await;
            }
            Err(error) => return Err(error),
        }
        let upstream_cost = price
            .price
            .upstream_cost(usage, true)
            .unwrap_or_else(|error| {
                tracing::error!(%error, hold_key,
                "pricing the metered event's upstream cost failed; the row records untracked");
                None
            });
        record_metered_usage(
            self,
            &crate::usage::UsageRow {
                request_id,
                tenant_id,
                key_id: None,
                model: watch.model.clone(),
                channel: watch.channel.clone(),
                price_version: watch.price_version,
                kind,
                entry_id: *entry_id_for(&settlement_key_for(hold_key)).as_uuid(),
                usage: usage.clone(),
                charged_minor: charged,
                freeze_minor: watch.freeze_minor,
                upstream_cost_minor: upstream_cost,
                upstream_attempts: 0,
            },
            hold_key,
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
            crate::billing::BillLine {
                item: "input".into(),
                units: 0,
                price_per_m: watch.input_price,
            },
            crate::billing::BillLine {
                item: "output".into(),
                units: 0,
                price_per_m: watch.output_price,
            },
        ];
        let description = MeteredSettlement {
            request: &watch.request_id,
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
                return settled_outcome(self, &wallet, hold_key).await;
            }
            Err(error) => return Err(error),
        }
        record_metered_usage(
            self,
            &crate::usage::UsageRow {
                request_id: watch.request_id.clone(),
                tenant_id,
                key_id: None,
                model: watch.model.clone(),
                channel: watch.channel.clone(),
                price_version: watch.price_version,
                kind: SettlementKind::Released,
                entry_id: *entry_id_for(&settlement_key_for(hold_key)).as_uuid(),
                usage,
                charged_minor: 0,
                freeze_minor: watch.freeze_minor,
                upstream_cost_minor: Some(0),
                upstream_attempts: 0,
            },
            hold_key,
        )
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
        let (tenant_id, wallet) = metered_wallet(self, tenants, organization_id).await?;
        check_event(event)?;
        let price = resolve_event_price(self, &event.channel, &event.billable_code, None).await?;
        check_usage(&event.usage, &price.price, &event.billable_code)?;
        let itemized = price.price.itemize(&event.usage, true)?;
        let discount_percent = self
            .discount_percent(organization_id, &event.billable_code)
            .await?
            .map(i64::from);
        let charged = match discount_percent {
            Some(percent) => itemized.discounted_minor(percent)?,
            None => itemized.total_minor()?,
        };
        if charged <= 0 {
            return Err(WalletError::InvalidInput(
                "the declared usage prices to zero".into(),
            ));
        }
        let hold_key = hold_key_for(idempotency_key)?;
        let request_id = event.event_id.clone().unwrap_or_else(|| hold_key.clone());
        // A one-shot hold is watched like a held one: a process dying between
        // the hold and the settle would otherwise leave the freeze pinned
        // forever — the sweeper releases it to zero like any orphaned hold.
        self.note_open_hold(&OpenHold {
            hold_key: hold_key.clone(),
            tenant_id: tenant_id.clone(),
            request_id: request_id.clone(),
            model: event.billable_code.clone(),
            channel: event.channel.clone(),
            price_version: price.version,
            input_price: price.price.input_price_per_million,
            output_price: price.price.output_price_per_million,
            freeze_minor: charged,
            key_id: None,
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
        let description = MeteredSettlement {
            request: &request_id,
            service: &service_name(service),
            channel: &event.channel,
            model: &event.billable_code,
            price_version: price.version,
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
                return settled_outcome(self, &wallet, &hold_key).await;
            }
            Err(error) => return Err(error),
        }
        let upstream_cost =
            price
                .price
                .upstream_cost(&event.usage, true)
                .unwrap_or_else(|error| {
                    tracing::error!(%error, hold_key,
                        "pricing the metered event's upstream cost failed; the row records untracked");
                    None
                });
        record_metered_usage(
            self,
            &crate::usage::UsageRow {
                request_id,
                tenant_id,
                key_id: None,
                model: event.billable_code.clone(),
                channel: event.channel.clone(),
                price_version: price.version,
                kind: SettlementKind::Usage,
                entry_id: *entry_id_for(&settlement_key_for(&hold_key)).as_uuid(),
                usage: event.usage.clone(),
                charged_minor: charged,
                freeze_minor: charged,
                upstream_cost_minor: upstream_cost,
                upstream_attempts: 0,
            },
            &hold_key,
        )
        .await;
        Ok(MeteredOutcome {
            charged_minor: charged,
            kind: SettlementKind::Usage,
            price_version: price.version,
            settlement_id: *entry_id_for(&settlement_key_for(&hold_key)).as_uuid(),
        })
    }
}
