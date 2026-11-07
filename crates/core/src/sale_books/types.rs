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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaleTerms {
    pub price: SalePrice,
    #[serde(default)]
    pub discount_percent: i64,
}
impl SaleTerms {
    pub fn validate(&self) -> Result<(), WalletError> {
        if !(0..=100).contains(&self.discount_percent) {
            return Err(super::invalid("discountPercent must be 0..=100"));
        }
        Ok(())
    }
}
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaleChangeRequest {
    pub idempotency_key: String,
    pub reason: String,
    pub change: SaleChange,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "operation",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum SaleChange {
    CreateBook {
        name: String,
    },
    SaveDraft {
        book_id: Uuid,
        expected_revision: i64,
        models: SaleModelMap,
    },
    PublishDraft {
        book_id: Uuid,
        draft_revision: i64,
    },
    ScheduleDefault {
        book_id: Uuid,
        version: i64,
        #[serde(default, with = "time::serde::rfc3339::option")]
        effective_from: Option<OffsetDateTime>,
        #[serde(default, with = "time::serde::rfc3339::option")]
        effective_until: Option<OffsetDateTime>,
        replaces: Option<Uuid>,
    },
    AssignBook {
        organization_id: Uuid,
        book_id: Uuid,
        version: i64,
        #[serde(default, with = "time::serde::rfc3339::option")]
        effective_from: Option<OffsetDateTime>,
        #[serde(default, with = "time::serde::rfc3339::option")]
        effective_until: Option<OffsetDateTime>,
        replaces: Option<Uuid>,
    },
    OverrideModel {
        organization_id: Uuid,
        model: String,
        terms: Box<SaleTerms>,
        #[serde(default, with = "time::serde::rfc3339::option")]
        effective_from: Option<OffsetDateTime>,
        #[serde(default, with = "time::serde::rfc3339::option")]
        effective_until: Option<OffsetDateTime>,
        replaces: Option<Uuid>,
    },
    EndSchedule {
        schedule_id: Uuid,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SaleMutationResult {
    pub id: Uuid,
    pub resource_id: Uuid,
    pub revision: Option<i64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaleBook {
    pub id: Uuid,
    pub name: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaleBookPage {
    pub books: Vec<SaleBook>,
    pub next_after: Option<Uuid>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaleDraft {
    pub revision: i64,
    pub models: SaleModelMap,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaleVersion {
    pub version: i64,
    pub draft_revision: i64,
    pub models: SaleModelMap,
    #[serde(with = "time::serde::rfc3339")]
    pub published_at: OffsetDateTime,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaleSchedule {
    pub id: Uuid,
    pub kind: String,
    pub organization_id: Option<Uuid>,
    pub model: Option<String>,
    pub book_id: Option<Uuid>,
    pub version: Option<i64>,
    pub terms: Option<SaleTerms>,
    #[serde(with = "time::serde::rfc3339")]
    pub effective_from: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub scheduled_until: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub effective_until: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaleBookHistory {
    pub book: SaleBook,
    pub drafts: Vec<SaleDraft>,
    pub versions: Vec<SaleVersion>,
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
    pub fn try_new(usage: UsageRecord) -> Result<Self, WalletError> {
        usage.validate()?;
        if usage.usage_details.is_some() || usage.end_user.is_some() || !usage.tags.is_empty() {
            return Err(super::invalid(
                "preview usage accepts normalized dimensions only",
            ));
        }
        Ok(Self(usage))
    }
    #[must_use]
    pub fn usage(&self) -> &UsageRecord {
        &self.0
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum SalePreviewSource {
    #[default]
    Effective,
    Draft {
        book_id: Uuid,
        revision: i64,
    },
    Legacy {
        channel: String,
        version: i64,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SalePreviewRequest {
    pub organization_id: Uuid,
    pub model: String,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub at: Option<OffsetDateTime>,
    #[serde(default)]
    pub source: SalePreviewSource,
    pub usage: SalePreviewUsage,
}
/// Pin this whole value when admitting a request. Later configuration is irrelevant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SalePriceSnapshot {
    pub business_id: Uuid,
    pub organization_id: Uuid,
    pub model: String,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    pub source: String,
    pub schedule_id: Option<Uuid>,
    pub book_id: Option<Uuid>,
    pub version: Option<i64>,
    pub draft_revision: Option<i64>,
    pub legacy_channel: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub effective_from: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub effective_until: Option<OffsetDateTime>,
    pub terms: SaleTerms,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SalePricePreview {
    pub snapshot: SalePriceSnapshot,
    pub lines: Vec<BillLine>,
    pub matched_rule: Option<RuleMatch>,
    pub undiscounted_minor: i64,
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
