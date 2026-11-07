//! Sale-book value types and the validation every write applies.

use crate::{BillLine, BusinessCapability, Price, RuleMatch, UsageRecord, WalletError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use time::OffsetDateTime;
use uuid::Uuid;

/// A sale price cannot contain supplier costs, including inside conditional rules.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct SalePrice(Price);
impl<'de> Deserialize<'de> for SalePrice {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(d)?;
        if value.get("upstream").is_some()
            || value
                .get("rules")
                .and_then(Value::as_array)
                .is_some_and(|rules| {
                    rules
                        .iter()
                        .any(|r| r.get("price").is_some_and(|p| p.get("upstream").is_some()))
                })
        {
            return Err(serde::de::Error::custom(
                "supplier costs are not sale terms",
            ));
        }
        let price: Price = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
        Self::try_new(price).map_err(serde::de::Error::custom)
    }
}
impl SalePrice {
    /// Validates `price` as sale terms: supplier costs are refused, at most
    /// 100 conditional rules are allowed, and service-tier labels are capped
    /// at 64 bytes.
    pub fn try_new(price: Price) -> Result<Self, WalletError> {
        if price.upstream.is_some() || price.rules.iter().any(|r| r.price.upstream.is_some()) {
            return Err(super::invalid("supplier costs are not sale terms"));
        }
        if price.rules.len() > 100
            || price
                .rules
                .iter()
                .any(|r| r.cond.service_tier.as_ref().is_some_and(|t| t.len() > 64))
        {
            return Err(super::invalid("sale rules exceed their limits"));
        }
        price.validate().map_err(WalletError::InvalidInput)?;
        Ok(Self(price))
    }
    /// Returns the underlying price list.
    #[must_use]
    pub fn price(&self) -> &Price {
        &self.0
    }
    pub(super) fn legacy(mut price: Price) -> Result<Self, WalletError> {
        price.upstream = None;
        for rule in &mut price.rules {
            rule.price.upstream = None;
        }
        Self::try_new(price)
    }
}

/// The price list and discount a sale book applies to one model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaleTerms {
    /// Per-dimension prices the sale charges.
    pub price: SalePrice,
    /// Whole-percent discount applied after itemization, in `0..=100`.
    #[serde(default)]
    pub discount_percent: i64,
}
impl SaleTerms {
    /// Checks that the discount percentage is within `0..=100`.
    pub fn validate(&self) -> Result<(), WalletError> {
        if !(0..=100).contains(&self.discount_percent) {
            return Err(super::invalid("discountPercent must be 0..=100"));
        }
        Ok(())
    }
}
/// Sale terms keyed by the model name a customer requests.
pub type SaleModelMap = BTreeMap<String, SaleTerms>;
pub(super) fn validate_models(models: &SaleModelMap) -> Result<(), WalletError> {
    if models.is_empty() || models.len() > 500 {
        return Err(super::invalid("a sale book needs 1 to 500 models"));
    }
    for (model, terms) in models {
        super::model_name(model)?;
        terms.validate()?;
    }
    Ok(())
}

/// One sale-book mutation, idempotent under `idempotency_key`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaleChangeRequest {
    /// Caller-chosen key; a replay with the same terms answers the standing
    /// mutation.
    pub idempotency_key: String,
    /// Operator-facing reason recorded in the audit trail.
    pub reason: String,
    /// The operation to apply.
    pub change: SaleChange,
}
/// A typed sale-book operation, serialized with an `operation` tag.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "operation",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum SaleChange {
    /// Creates an empty sale book.
    CreateBook {
        /// Human-readable book name, unique within the business.
        name: String,
    },
    /// Replaces a book's full draft model map at an expected revision.
    SaveDraft {
        /// Book being edited.
        book_id: Uuid,
        /// Revision the caller last read; a mismatch is a conflict.
        expected_revision: i64,
        /// Complete draft terms keyed by model name.
        models: SaleModelMap,
    },
    /// Publishes a draft revision as an immutable book version.
    PublishDraft {
        /// Book owning the draft.
        book_id: Uuid,
        /// Draft revision to publish.
        draft_revision: i64,
    },
    /// Schedules a book version as the business default for a time window.
    ScheduleDefault {
        /// Book to schedule.
        book_id: Uuid,
        /// Published version to make effective.
        version: i64,
        /// Start of the window; `None` starts immediately.
        #[serde(default, with = "time::serde::rfc3339::option")]
        effective_from: Option<OffsetDateTime>,
        /// End of the window; `None` runs until replaced.
        #[serde(default, with = "time::serde::rfc3339::option")]
        effective_until: Option<OffsetDateTime>,
        /// Schedule this one supersedes, if any.
        replaces: Option<Uuid>,
    },
    /// Assigns a book version to a customer organization for a time window.
    AssignBook {
        /// Customer organization the schedule applies to.
        organization_id: Uuid,
        /// Book to assign.
        book_id: Uuid,
        /// Published version to assign.
        version: i64,
        /// Start of the assignment; `None` starts immediately.
        #[serde(default, with = "time::serde::rfc3339::option")]
        effective_from: Option<OffsetDateTime>,
        /// End of the assignment; `None` runs until replaced.
        #[serde(default, with = "time::serde::rfc3339::option")]
        effective_until: Option<OffsetDateTime>,
        /// Schedule this one supersedes, if any.
        replaces: Option<Uuid>,
    },
    /// Overrides one model's terms for a customer organization.
    OverrideModel {
        /// Customer organization the override applies to.
        organization_id: Uuid,
        /// Model name the override applies to.
        model: String,
        /// Sale terms for the override.
        terms: Box<SaleTerms>,
        /// Start of the override; `None` starts immediately.
        #[serde(default, with = "time::serde::rfc3339::option")]
        effective_from: Option<OffsetDateTime>,
        /// End of the override; `None` runs until replaced.
        #[serde(default, with = "time::serde::rfc3339::option")]
        effective_until: Option<OffsetDateTime>,
        /// Schedule this one supersedes, if any.
        replaces: Option<Uuid>,
    },
    /// Ends a schedule at a chosen time.
    EndSchedule {
        /// Schedule to end.
        schedule_id: Uuid,
        /// Instant the schedule stops applying.
        #[serde(with = "time::serde::rfc3339")]
        effective_until: OffsetDateTime,
    },
}
impl SaleChange {
    pub(super) fn capability(&self) -> BusinessCapability {
        match self {
            Self::AssignBook { .. } | Self::OverrideModel { .. } => {
                BusinessCapability::CommercialWrite
            }
            _ => BusinessCapability::CatalogWrite,
        }
    }
    pub(super) fn action(&self) -> &'static str {
        match self {
            Self::CreateBook { .. } => "business.sale.book.create",
            Self::SaveDraft { .. } => "business.sale.draft.save",
            Self::PublishDraft { .. } => "business.sale.draft.publish",
            Self::ScheduleDefault { .. } => "business.sale.default.schedule",
            Self::AssignBook { .. } => "business.sale.customer.assign",
            Self::OverrideModel { .. } => "business.sale.customer.override",
            Self::EndSchedule { .. } => "business.sale.schedule.end",
        }
    }
}

/// Identifiers produced by one sale mutation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SaleMutationResult {
    /// Receipt id of the mutation.
    pub id: Uuid,
    /// Book or schedule the mutation affected.
    pub resource_id: Uuid,
    /// New revision, when the operation produces one.
    pub revision: Option<i64>,
}
/// A sale book: a named, versioned set of sale terms.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaleBook {
    /// Book identifier.
    pub id: Uuid,
    /// Human-readable name, unique within the business.
    pub name: String,
    /// When the book was created.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}
/// One page of sale books.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaleBookPage {
    /// Books in this page.
    pub books: Vec<SaleBook>,
    /// Cursor for the next page; `None` at the end.
    pub next_after: Option<Uuid>,
}
/// A mutable draft of one book's model terms.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaleDraft {
    /// Draft revision number, increasing per book.
    pub revision: i64,
    /// Terms keyed by model name.
    pub models: SaleModelMap,
    /// When the draft revision was written.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}
/// An immutable published rendition of a book's models.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaleVersion {
    /// Version number, increasing per book.
    pub version: i64,
    /// Draft revision this version was published from.
    pub draft_revision: i64,
    /// Terms keyed by model name.
    pub models: SaleModelMap,
    /// When the version was published.
    #[serde(with = "time::serde::rfc3339")]
    pub published_at: OffsetDateTime,
}
/// One effective-time window for a default book, customer assignment or
/// per-model override.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaleSchedule {
    /// Schedule identifier.
    pub id: Uuid,
    /// Schedule kind: `default`, `assigned` or `override`.
    pub kind: String,
    /// Customer organization for an assignment or override; `None` for a
    /// default schedule.
    pub organization_id: Option<Uuid>,
    /// Model name for an override; `None` otherwise.
    pub model: Option<String>,
    /// Book the schedule names, when it names one.
    pub book_id: Option<Uuid>,
    /// Book version the schedule pins.
    pub version: Option<i64>,
    /// Inline terms for a model override.
    pub terms: Option<SaleTerms>,
    /// First instant the schedule applies.
    #[serde(with = "time::serde::rfc3339")]
    pub effective_from: OffsetDateTime,
    /// Planned end supplied when the schedule was created; `None` for an
    /// open-ended schedule.
    #[serde(with = "time::serde::rfc3339::option")]
    pub scheduled_until: Option<OffsetDateTime>,
    /// Instant the schedule stopped applying: an explicit end, else the
    /// planned one; `None` while it still applies.
    #[serde(with = "time::serde::rfc3339::option")]
    pub effective_until: Option<OffsetDateTime>,
    /// When the schedule was recorded.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}
/// A book's drafts, versions and schedules.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaleBookHistory {
    /// The book these records belong to.
    pub book: SaleBook,
    /// Draft revisions, in revision order.
    pub drafts: Vec<SaleDraft>,
    /// Published versions, in version order.
    pub versions: Vec<SaleVersion>,
    /// Schedules referencing the book, by effective start.
    pub schedules: Vec<SaleSchedule>,
}

/// Preview accepts only named normalized dimensions, never a free-form provider blob.
#[derive(Debug, Clone)]
pub struct SalePreviewUsage(UsageRecord);
impl Serialize for SalePreviewUsage {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut value = serde_json::to_value(&self.0).map_err(serde::ser::Error::custom)?;
        if let Some(object) = value.as_object_mut() {
            for name in ["endUser", "tags", "usageDetails"] {
                object.remove(name);
            }
        }
        value.serialize(s)
    }
}
impl<'de> Deserialize<'de> for SalePreviewUsage {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(d)?;
        let object = value
            .as_object()
            .ok_or_else(|| serde::de::Error::custom("usage must be an object"))?;
        if object.keys().any(|k| {
            !matches!(
                k.as_str(),
                "inputTokens"
                    | "outputTokens"
                    | "cachedTokens"
                    | "cacheWrite5mTokens"
                    | "cacheWrite1hTokens"
                    | "reasoningTokens"
                    | "toolCalls"
                    | "imageInputTokens"
                    | "audioInputTokens"
                    | "videoInputTokens"
                    | "imageOutputTokens"
                    | "audioOutputTokens"
                    | "serviceTier"
                    | "eventType"
            )
        }) {
            return Err(serde::de::Error::custom("unknown sale usage dimension"));
        }
        let usage: UsageRecord = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
        usage.validate().map_err(serde::de::Error::custom)?;
        Ok(Self(usage))
    }
}
impl SalePreviewUsage {
    /// Validates `usage` and accepts only normalized dimensions: provider
    /// details, end-user identity and tags are refused.
    pub fn try_new(usage: UsageRecord) -> Result<Self, WalletError> {
        usage.validate()?;
        if usage.usage_details.is_some() || usage.end_user.is_some() || !usage.tags.is_empty() {
            return Err(super::invalid(
                "preview usage accepts normalized dimensions only",
            ));
        }
        Ok(Self(usage))
    }
    /// Returns the normalized usage record.
    #[must_use]
    pub fn usage(&self) -> &UsageRecord {
        &self.0
    }
}
/// Where a preview takes its sale terms from.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum SalePreviewSource {
    /// The terms effective for the organization at the requested instant.
    #[default]
    Effective,
    /// An unpublished draft revision of a book.
    Draft {
        /// Book holding the draft.
        book_id: Uuid,
        /// Draft revision to rate.
        revision: i64,
    },
    /// A legacy per-channel price list version.
    Legacy {
        /// Channel name.
        channel: String,
        /// Channel price version number, `1..=2147483647`.
        version: i64,
    },
}
/// A customer-scoped request to quote one usage record against sale terms.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SalePreviewRequest {
    /// Customer whose sale terms are consulted.
    pub organization_id: Uuid,
    /// Model name to rate.
    pub model: String,
    /// Instant the effective terms are resolved at; `None` uses now.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub at: Option<OffsetDateTime>,
    /// Where to take the terms from.
    #[serde(default)]
    pub source: SalePreviewSource,
    /// The normalized usage dimensions to price.
    pub usage: SalePreviewUsage,
}
/// Pin this whole value when admitting a request. Later configuration is irrelevant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SalePriceSnapshot {
    /// Business whose sale configuration produced these terms.
    pub business_id: Uuid,
    /// Customer organization the terms were resolved for.
    pub organization_id: Uuid,
    /// Model name the terms price.
    pub model: String,
    /// Instant the terms were effective.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// Source kind: `default`, `assigned`, `override`, `draft` or `legacy`.
    pub source: String,
    /// Schedule the terms came from, if any.
    pub schedule_id: Option<Uuid>,
    /// Book the terms came from, if a book supplied them.
    pub book_id: Option<Uuid>,
    /// Published book or legacy channel version, when applicable.
    pub version: Option<i64>,
    /// Draft revision when the source is a draft.
    pub draft_revision: Option<i64>,
    /// Channel name when the source is a legacy price list.
    pub legacy_channel: Option<String>,
    /// First instant the terms apply.
    #[serde(with = "time::serde::rfc3339")]
    pub effective_from: OffsetDateTime,
    /// Instant the terms stop applying; `None` for open-ended.
    #[serde(with = "time::serde::rfc3339::option")]
    pub effective_until: Option<OffsetDateTime>,
    /// The resolved sale terms.
    pub terms: SaleTerms,
}
/// The exact result of rating one usage record against pinned sale terms.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SalePricePreview {
    /// The terms the quote was computed from.
    pub snapshot: SalePriceSnapshot,
    /// Itemized bill lines making up the charge.
    pub lines: Vec<BillLine>,
    /// Conditional price rule that applied, if any.
    pub matched_rule: Option<RuleMatch>,
    /// Itemized total before the discount, in minor units.
    pub undiscounted_minor: i64,
    /// Amount to charge after the discount, in minor units.
    pub charge_minor: i64,
}
impl SalePriceSnapshot {
    /// Pure exact quote. No store, provider route, clock or current discounts are read.
    pub fn preview(&self, usage: &UsageRecord) -> Result<SalePricePreview, WalletError> {
        self.terms.validate()?;
        usage.validate()?;
        if usage.usage_details.is_some() {
            return Err(super::invalid(
                "sale rating requires normalized dimensions, not provider details",
            ));
        }
        let price = self.terms.price.price();
        let unpriced = price.unpriced_dimensions(usage);
        if !unpriced.is_empty() {
            return Err(super::invalid(&format!(
                "unpriced sale dimensions: {}",
                unpriced.join(", ")
            )));
        }
        let itemized = price.itemize(usage, true)?;
        let undiscounted_minor = itemized.total_minor()?;
        let charge_minor = if self.terms.discount_percent == 0 {
            undiscounted_minor
        } else {
            itemized.discounted_minor(self.terms.discount_percent)?
        };
        Ok(SalePricePreview {
            snapshot: self.clone(),
            lines: itemized.lines,
            matched_rule: itemized.matched_rule,
            undiscounted_minor,
            charge_minor,
        })
    }
}
