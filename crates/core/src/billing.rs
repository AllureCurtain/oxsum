//! What a gateway turn costs: the freeze taken before upstream is contacted, the charge settled
//! after it answers, and the estimation that stands in for usage when upstream never reported any.
//!
//! Everything here is integer arithmetic in minor units. A price is quoted per million tokens, so a
//! price times a token count is a product of two large integers and both divisions round **up**: the
//! freeze must never come out below what the turn can cost, and the charge must never come out above
//! the freeze. Nothing on this path is floating point.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::error::WalletError;
use crate::usage::UsageRecord;

/// Tokens a price is quoted per.
const PER_MILLION: i64 = 1_000_000;

/// Bytes added per message to the input upper bound: the role, the delimiters and the newlines a
/// chat message costs on top of its text. Sixteen is a deliberate overshoot of the handful of bytes
/// a real format adds; every byte of overshoot is refunded at settlement.
const PER_MESSAGE_OVERHEAD: i64 = 16;

// ── prices ───────────────────────────────────────────────────────────────────

/// One model's price, in minor units per million tokens, and how much output it may produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Price {
    /// Minor units per million input tokens. Zero is a model that is free to prompt.
    #[serde(rename = "inputPricePerMillion")]
    pub input_per_million: i64,
    /// Minor units per million output tokens.
    #[serde(rename = "outputPricePerMillion")]
    pub output_per_million: i64,
    /// The most output the model can produce. A request asking for more, or for nothing, gets this.
    #[serde(rename = "maxOutputTokens")]
    pub max_output_tokens: i64,
}

impl Price {
    /// Checks the parts of a price that no arithmetic downstream can repair.
    ///
    /// # Errors
    ///
    /// Names the field when a rate is negative or the output ceiling is not positive.
    pub fn validate(&self) -> Result<(), String> {
        if self.input_per_million < 0 || self.output_per_million < 0 {
            return Err("prices must be zero or more minor units per million tokens".into());
        }
        if self.max_output_tokens <= 0 {
            return Err("maxOutputTokens must be positive".into());
        }
        Ok(())
    }

    /// The output upper bound for one request: what the caller asked for, or the model's ceiling,
    /// and never more than the ceiling — the freeze is only a promise if upstream cannot exceed it.
    ///
    /// # Errors
    ///
    /// Refuses a `max_tokens` of zero or less; the caller asked for a completion it cannot get.
    pub fn output_upper_bound(&self, asked: Option<i64>) -> Result<i64, WalletError> {
        let asked = asked.unwrap_or(self.max_output_tokens);
        if asked <= 0 {
            return Err(WalletError::InvalidInput(
                "max_tokens must be positive".into(),
            ));
        }
        Ok(asked.min(self.max_output_tokens))
    }

    /// The most this request could cost: the input upper bound at the input price plus the output
    /// upper bound at the output price, rounded up to the minor unit.
    ///
    /// # Errors
    ///
    /// Refuses a `max_tokens` of zero or less, and an amount that does not fit in 64 bits.
    pub fn freeze_minor(
        &self,
        texts: &[&str],
        asked_output: Option<i64>,
    ) -> Result<i64, WalletError> {
        let input = input_upper_bound(texts);
        let output = self.output_upper_bound(asked_output)?;
        self.minor_for(input, output)
    }

    /// What a turn cost at this price, before the freeze caps it.
    ///
    /// # Errors
    ///
    /// Refuses usage that does not fit in 64 bits once priced.
    pub fn cost_minor(&self, usage: &UsageRecord) -> Result<i64, WalletError> {
        self.minor_for(usage.input_tokens, usage.output_tokens)
    }

    /// The price of a token count, rounded up: a fraction of a minor unit is still a minor unit of
    /// cost, and rounding down would hand out free credit one request at a time.
    fn minor_for(&self, input_tokens: i64, output_tokens: i64) -> Result<i64, WalletError> {
        // i128 so the multiplication cannot overflow before the division brings it back down.
        let numerator = i128::from(input_tokens) * i128::from(self.input_per_million)
            + i128::from(output_tokens) * i128::from(self.output_per_million);
        // Ceiling division, by hand: both sides are non-negative (validated above), and
        // `i128::div_ceil` is still unstable.
        let per_million = i128::from(PER_MILLION);
        let minor = (numerator + per_million - 1) / per_million;
        i64::try_from(minor)
            .map_err(|_| WalletError::InvalidInput("the amount does not fit in 64 bits".into()))
    }
}

/// The input upper bound in tokens: the UTF-8 byte count of every text, plus a fixed per-message
/// overhead.
///
/// Mainstream tokenizers are byte-level BPE, so one token spans at least one byte and the byte count
/// never undercounts tokens. The cost is overshooting — roughly fourfold for English, twofold for
/// Chinese — and every byte of excess is refunded at settlement.
#[must_use]
pub fn input_upper_bound(texts: &[&str]) -> i64 {
    texts
        .iter()
        .map(|text| text.len() as i64 + PER_MESSAGE_OVERHEAD)
        .sum()
}

// ── usage ────────────────────────────────────────────────────────────────────

/// Estimates tokens with tiktoken's `o200k_base`, for the path where upstream reported no usage.
///
/// The vocabulary is built on first use and kept for the process: a few hundred milliseconds and a
/// few megabytes, paid once, on a path that should be rare. This is a fallback only — a turn that
/// got usage from upstream is never estimated, so the two pricing paths do not mix.
#[must_use]
pub fn estimate_tokens(texts: &[&str]) -> i64 {
    static BPE: OnceLock<Option<tiktoken_rs::CoreBPE>> = OnceLock::new();
    let bpe = BPE.get_or_init(|| tiktoken_rs::o200k_base().ok());
    match bpe {
        Some(bpe) => texts
            .iter()
            .map(|text| bpe.encode_ordinary(text).len() as i64)
            .sum(),
        // A vocabulary that will not build is not a reason to fail a settlement: for a byte-level
        // tokenizer the byte count is an upper bound on the token count, which is all this needs.
        None => input_upper_bound(texts),
    }
}

// ── what the ledger records ──────────────────────────────────────────────────

/// The settlement types of product.md's table, as they are written into the entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettlementKind {
    /// Upstream reported usage, and it was priced.
    Usage,
    /// Upstream reported no usage; the turn was priced from a local estimate.
    Estimated,
    /// The client disconnected mid-stream: the upstream call was cancelled and the forwarded part
    /// was priced from a local estimate.
    ClientCancelled,
    /// Upstream answered with an error before emitting anything. Nothing was charged.
    UpstreamError,
    /// Upstream could not be reached at all. Nothing was charged.
    UpstreamUnreachable,
    /// Upstream's usage priced above the freeze. The freeze was charged and the excess is an anomaly.
    Capped,
    /// The hold timed out with no settlement (e.g. the gateway crashed). Nothing was charged:
    /// the sweeper released the whole freeze, and the record marks the anomaly for the admin page.
    Swept,
}

impl SettlementKind {
    /// The spelling a usage row stores for the kind: the record's own serde form.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Usage => "usage",
            Self::Estimated => "estimated",
            Self::ClientCancelled => "client_cancelled",
            Self::UpstreamError => "upstream_error",
            Self::UpstreamUnreachable => "upstream_unreachable",
            Self::Capped => "capped",
            Self::Swept => "swept",
        }
    }
}

/// What a settlement entry records, serialised into its description.
///
/// The description is part of the entry and covered by its content hash, so a bill proves not only
/// what was charged but by how many tokens at what price — the arithmetic is verifiable, not just
/// the total. It is built only from what the request itself decided (the request id, the channel and
/// the price version it started on, the model, the prices, the counts), never from a clock or a
/// counter, so a retry under the same idempotency key reproduces the same entry instead of colliding
/// with it.
///
/// The channel and the version are what keep those prices checkable later: a price change appends a
/// version rather than replacing one (docs/product.md, "Channels and prices"), so "this turn was
/// priced by version 3" stays a statement about a row that is still there.
/// The version the settlement description writes on the wire, and the only
/// version [`SettlementRecord::parse`] reads. A record that names another one —
/// older or newer — is not this build's to interpret: `oxsum_verify`'s
/// `verify_charge` dispatches on `v` and leaves those to inclusion proof alone
/// (docs/decisions.md, T1-2).
const DESCRIPTION_VERSION: i64 = 2;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settlement<'a> {
    /// The request id from `x-oxsum-request-id`; the ledger keys are derived from it.
    pub request: &'a str,
    /// The channel that served the request.
    pub channel: &'a str,
    /// The model the caller asked for.
    pub model: &'a str,
    /// The price version in force when the request started.
    pub price_version: i64,
    /// How this turn was priced.
    pub kind: SettlementKind,
    /// What the turn used. Written into the record as [`MeteredUsage`]: the
    /// metered dimensions only, because `end_user`, `tags` and
    /// `usage_details.provider_raw` may carry caller or prompt data — they live
    /// in the mutable usage row and never reach an immutable ledger description
    /// (docs/decisions.md, data retention).
    pub usage: &'a UsageRecord,
    /// Minor units per million input tokens at the time of the request.
    pub input_price: i64,
    /// Minor units per million output tokens at the time of the request.
    pub output_price: i64,
    /// What was charged, never more than `freeze`.
    pub charged: i64,
    /// What was frozen before the call.
    pub freeze: i64,
}

/// Whether a metered count is zero: zero fields are not written, so the common
/// text-only description stays small inside the ledger's 512-character limit.
fn is_zero(count: &i64) -> bool {
    *count == 0
}

/// A usage record's metered dimensions, as a settlement description carries
/// them (v2's `usage`). A projection of [`UsageRecord`] minus its attribution:
/// `end_user`, `tags` and `usage_details` are deliberately absent — see
/// [`Settlement::usage`]. `service_tier` and `event_type` are here, not there:
/// prices may match on them, so a verifier recomputing the charge needs them
/// under the hash.
///
/// Zero fields are not written, so a text-only turn stays compact inside the
/// ledger's 512-character description limit; they read back as zero.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MeteredUsage {
    /// Prompt-side tokens, *including* the cached subset.
    #[serde(skip_serializing_if = "is_zero")]
    pub input_tokens: i64,
    /// Completion-side tokens, *including* the reasoning subset.
    #[serde(skip_serializing_if = "is_zero")]
    pub output_tokens: i64,
    /// The part of `input_tokens` billed at the cache-read price when one exists.
    #[serde(skip_serializing_if = "is_zero")]
    pub cached_tokens: i64,
    /// Tokens written into the 5-minute provider cache tier.
    #[serde(skip_serializing_if = "is_zero")]
    pub cache_write_5m_tokens: i64,
    /// Tokens written into the 1-hour provider cache tier.
    #[serde(skip_serializing_if = "is_zero")]
    pub cache_write_1h_tokens: i64,
    /// The part of `output_tokens` that is model reasoning.
    #[serde(skip_serializing_if = "is_zero")]
    pub reasoning_tokens: i64,
    /// Billable tool invocations.
    #[serde(skip_serializing_if = "is_zero")]
    pub tool_calls: i64,
    /// Media tokens, priced on their own lines when the price book prices them.
    #[serde(skip_serializing_if = "is_zero")]
    pub image_input_tokens: i64,
    /// Audio input tokens.
    #[serde(skip_serializing_if = "is_zero")]
    pub audio_input_tokens: i64,
    /// Video input tokens.
    #[serde(skip_serializing_if = "is_zero")]
    pub video_input_tokens: i64,
    /// Image output tokens.
    #[serde(skip_serializing_if = "is_zero")]
    pub image_output_tokens: i64,
    /// Audio output tokens.
    #[serde(skip_serializing_if = "is_zero")]
    pub audio_output_tokens: i64,
    /// The service tier the caller asked for: a pricing slot, not attribution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    /// The billable event kind (`None` for a native gateway turn).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_type: Option<String>,
}

impl From<&UsageRecord> for MeteredUsage {
    /// The record's metered half: every priced dimension, none of its attribution.
    fn from(usage: &UsageRecord) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cached_tokens: usage.cached_tokens,
            cache_write_5m_tokens: usage.cache_write_5m_tokens,
            cache_write_1h_tokens: usage.cache_write_1h_tokens,
            reasoning_tokens: usage.reasoning_tokens,
            tool_calls: usage.tool_calls,
            image_input_tokens: usage.image_input_tokens,
            audio_input_tokens: usage.audio_input_tokens,
            video_input_tokens: usage.video_input_tokens,
            image_output_tokens: usage.image_output_tokens,
            audio_output_tokens: usage.audio_output_tokens,
            service_tier: usage.service_tier.clone(),
            event_type: usage.event_type.clone(),
        }
    }
}

/// One priced dimension of a settlement: what was counted, and the rate it was
/// priced at. `price_per_m` is minor units per million units; `charged` is the
/// whole lines' summed cost ceiling-divided by a million, capped by `freeze` —
/// the rule `oxsum_verify::verify_charge` recomputes (v2's `lines`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BillLine {
    /// The dimension this line prices (`"input"`, `"output"`; more arrive with
    /// the itemized price book, roadmap P1-4).
    pub item: String,
    /// How much of it the turn used, in the item's own units.
    pub units: i64,
    /// Minor units per million units.
    pub price_per_m: i64,
}

/// A settlement's description on the wire, versioned.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SettlementV2<'a> {
    /// The schema version — [`DESCRIPTION_VERSION`]. Spelled `v` on the wire.
    v: i64,
    request: &'a str,
    channel: &'a str,
    model: &'a str,
    price_version: i64,
    kind: SettlementKind,
    usage: MeteredUsage,
    lines: [BillLine; 2],
    charged: i64,
    freeze: i64,
}

impl Settlement<'_> {
    /// The compact JSON the settlement entry's description carries: a
    /// self-contained billing credential — every input the charge recomputes
    /// from is inside the content hash (docs/decisions.md, T1-2).
    ///
    /// # Errors
    ///
    /// Refuses a description that cannot be serialised, which cannot happen for these fields; the
    /// `Result` keeps the caller honest rather than unwrapping a formality.
    pub fn description(&self) -> Result<String, WalletError> {
        serde_json::to_string(&SettlementV2 {
            v: DESCRIPTION_VERSION,
            request: self.request,
            channel: self.channel,
            model: self.model,
            price_version: self.price_version,
            kind: self.kind,
            usage: MeteredUsage::from(self.usage),
            lines: [
                BillLine {
                    item: "input".into(),
                    units: self.usage.input_tokens,
                    price_per_m: self.input_price,
                },
                BillLine {
                    item: "output".into(),
                    units: self.usage.output_tokens,
                    price_per_m: self.output_price,
                },
            ],
            charged: self.charged,
            freeze: self.freeze,
        })
        .map_err(|error| WalletError::InvalidInput(format!("settlement record: {error}")))
    }
}

/// A settlement entry's description, read back into owned fields.
///
/// The reader of [`Settlement`], in the same module as the writer so the two shapes are
/// one definition: the requests page lists a request out of its settlement entry, and the
/// fields it shows (the model, the token counts, the charge) are the ones the entry's
/// content hash covers. The round trip is pinned by
/// `a_settlement_description_reads_back`.
///
/// A hold's own record does not parse into this type: it carries no channel, no token
/// counts and no prices, and its `kind` (`"hold"`) is not a [`SettlementKind`]. A
/// settlement written through the wallet API carries no description at all, and a record
/// naming a `v` other than this build's is left to the verifier's dispatch, not guessed
/// at here. Neither is read as a gateway request.
#[derive(Debug, Clone)]
pub struct SettlementRecord {
    /// The request id from `x-oxsum-request-id`; the ledger keys are derived from it.
    pub request: String,
    /// The channel that served the request.
    pub channel: String,
    /// The model the caller asked for.
    pub model: String,
    /// The price version in force when the request started.
    pub price_version: i64,
    /// How this turn was priced, in the record's own spelling.
    pub kind: SettlementKind,
    /// Tokens billed as input — `usage.input_tokens`, kept flat for the lists.
    pub input_tokens: i64,
    /// Tokens billed as output — `usage.output_tokens`.
    pub output_tokens: i64,
    /// Minor units per million input tokens at the time of the request — the
    /// `input` line's rate.
    pub input_price: i64,
    /// Minor units per million output tokens at the time of the request — the
    /// `output` line's rate.
    pub output_price: i64,
    /// What was charged, never more than `freeze`.
    pub charged: i64,
    /// What was frozen before the call.
    pub freeze: i64,
    /// The metered usage the turn was priced on, every dimension.
    pub usage: MeteredUsage,
    /// The priced lines the charge is the sum of.
    pub lines: Vec<BillLine>,
}

/// The v2 wire [`SettlementRecord::parse`] reads: [`SettlementV2`] owned.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettlementRecordV2 {
    v: i64,
    request: String,
    channel: String,
    model: String,
    price_version: i64,
    kind: SettlementKind,
    usage: MeteredUsage,
    lines: Vec<BillLine>,
    charged: i64,
    freeze: i64,
}

impl SettlementRecord {
    /// The record one entry's description carries, or `None` when the description is not a
    /// settlement's — a hold's record, an empty description, a version this build does not
    /// write, or text from a writer that is not this one.
    #[must_use]
    pub fn parse(description: &str) -> Option<Self> {
        let wire: SettlementRecordV2 = serde_json::from_str(description).ok()?;
        if wire.v != DESCRIPTION_VERSION {
            return None;
        }
        let price_of = |item: &str| {
            wire.lines
                .iter()
                .find(|line| line.item == item)
                .map(|line| line.price_per_m)
        };
        Some(Self {
            input_tokens: wire.usage.input_tokens,
            output_tokens: wire.usage.output_tokens,
            input_price: price_of("input")?,
            output_price: price_of("output")?,
            request: wire.request,
            channel: wire.channel,
            model: wire.model,
            price_version: wire.price_version,
            kind: wire.kind,
            charged: wire.charged,
            freeze: wire.freeze,
            usage: wire.usage,
            lines: wire.lines,
        })
    }
}

/// What the hold entry records, so a bill can pair the freeze with the request that caused it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct HoldRecord<'a> {
    request: &'a str,
    model: &'a str,
    freeze: i64,
    kind: &'static str,
}

/// The hold entry's description: which request froze the credit, for which model, and how much.
///
/// # Errors
///
/// As [`Settlement::description`].
pub fn hold_description(
    request: &str,
    model: &str,
    freeze_minor: i64,
) -> Result<String, WalletError> {
    serde_json::to_string(&HoldRecord {
        request,
        model,
        freeze: freeze_minor,
        kind: "hold",
    })
    .map_err(|error| WalletError::InvalidInput(format!("hold record: {error}")))
}

// ── the price book ───────────────────────────────────────────────────────────

/// The models this deployment serves, and the channel behind them.
///
/// In v1 one model maps to exactly one channel (product.md), so this is a flat map. TODO item 3
/// replaces the source with versioned rows and an admin UI; the gateway depends on this value rather
/// than on the environment, so that is a change of source rather than a rewrite.
#[derive(Debug, Clone)]
pub struct PriceBook {
    channel: String,
    models: BTreeMap<String, Price>,
}

impl PriceBook {
    /// Builds a book around models already in hand.
    #[must_use]
    pub fn new(channel: impl Into<String>, models: BTreeMap<String, Price>) -> Self {
        Self {
            channel: channel.into(),
            models,
        }
    }

    /// Parses `OXSUM_MODELS`: `{"model": {"inputPricePerMillion": …, "outputPricePerMillion": …,
    /// "maxOutputTokens": …}}`, with prices in minor units.
    ///
    /// # Errors
    ///
    /// Refuses malformed JSON, an unknown field, no models at all, an empty model name, and any
    /// price [`Price::validate`] refuses. The server refuses to start rather than guess a price.
    pub fn from_json(channel: &str, json: &str) -> Result<Self, String> {
        let models: BTreeMap<String, Price> = serde_json::from_str(json)
            .map_err(|error| format!("OXSUM_MODELS is not a model-to-price object: {error}"))?;
        if models.is_empty() {
            return Err("OXSUM_MODELS must configure at least one model".into());
        }
        for (model, price) in &models {
            if model.trim().is_empty() {
                return Err("OXSUM_MODELS has an empty model name".into());
            }
            price
                .validate()
                .map_err(|error| format!("model {model}: {error}"))?;
        }
        Ok(Self::new(channel, models))
    }

    /// The price of a model, or `None` when this deployment does not serve it.
    #[must_use]
    pub fn get(&self, model: &str) -> Option<&Price> {
        self.models.get(model)
    }

    /// The models, by name, in a stable order.
    pub fn models(&self) -> impl Iterator<Item = (&str, &Price)> {
        self.models
            .iter()
            .map(|(name, price)| (name.as_str(), price))
    }

    /// The channel's name, as `/v1/models` reports it in `owned_by`.
    #[must_use]
    pub fn channel(&self) -> &str {
        &self.channel
    }
}

#[cfg(test)]
mod tests {
    // The tests may unwrap: a panic here is a failing test, which is what a test is for.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// One credit per million tokens each way, eight thousand tokens of output.
    fn price() -> Price {
        Price {
            input_per_million: 1_000_000,
            output_per_million: 2_000_000,
            max_output_tokens: 8_000,
        }
    }

    #[test]
    fn the_input_bound_counts_bytes_and_message_overhead() {
        assert_eq!(input_upper_bound(&[]), 0);
        assert_eq!(input_upper_bound(&[""]), PER_MESSAGE_OVERHEAD);
        // "héllo" is six bytes in UTF-8, not five characters: bytes are what bounds tokens.
        assert_eq!(input_upper_bound(&["héllo"]), 6 + PER_MESSAGE_OVERHEAD);
        assert_eq!(input_upper_bound(&["a", "b"]), 2 + 2 * PER_MESSAGE_OVERHEAD);
    }

    #[test]
    fn the_output_bound_is_the_callers_capped_at_the_models() {
        assert_eq!(price().output_upper_bound(None).unwrap(), 8_000);
        assert_eq!(price().output_upper_bound(Some(100)).unwrap(), 100);
        assert_eq!(price().output_upper_bound(Some(1_000_000)).unwrap(), 8_000);
        assert!(price().output_upper_bound(Some(0)).is_err());
        assert!(price().output_upper_bound(Some(-1)).is_err());
    }

    #[test]
    fn the_freeze_rounds_up_and_covers_the_whole_turn() {
        // 84 bytes + overhead = 100 input tokens at 1 credit per million (one minor unit each),
        // 100 output tokens at 2 credits per million (two minor units each).
        let freeze = price()
            .freeze_minor(&["x".repeat(84).as_str()], Some(100))
            .unwrap();
        assert_eq!(freeze, 100 + 200);
    }

    #[test]
    fn a_fraction_of_a_minor_unit_rounds_up() {
        let cheap = Price {
            input_per_million: 1,
            output_per_million: 0,
            max_output_tokens: 1,
        };
        // An empty message is still the per-message overhead in input tokens, which at one minor
        // unit per million tokens is a millionth of a minor unit — and that still rounds up to one.
        assert_eq!(cheap.freeze_minor(&[""], None).unwrap(), 1);
    }

    #[test]
    fn usage_is_priced_with_the_same_rounding() {
        let usage = UsageRecord::tokens(1_000_000, 500_000).unwrap();
        assert_eq!(price().cost_minor(&usage).unwrap(), 1_000_000 + 1_000_000);
        assert!(UsageRecord::tokens(-1, 0).is_err());
        // A price high enough to overflow i64 once multiplied is refused, not wrapped.
        let absurd = Price {
            input_per_million: i64::MAX,
            output_per_million: i64::MAX,
            max_output_tokens: 1,
        };
        assert!(
            absurd
                .cost_minor(&UsageRecord::tokens(i64::MAX, i64::MAX).unwrap())
                .is_err()
        );
    }

    #[test]
    fn the_description_carries_the_arithmetic() {
        let usage = UsageRecord::tokens(116, 100).unwrap();
        let settlement = Settlement {
            request: "abc",
            channel: "deepseek",
            model: "deepseek-chat",
            price_version: 3,
            kind: SettlementKind::Usage,
            usage: &usage,
            input_price: 1_000_000,
            output_price: 2_000_000,
            charged: 316,
            freeze: 400,
        };
        let json = settlement.description().unwrap();
        assert_eq!(
            json,
            r#"{"v":2,"request":"abc","channel":"deepseek","model":"deepseek-chat","priceVersion":3,"kind":"usage","usage":{"inputTokens":116,"outputTokens":100},"lines":[{"item":"input","units":116,"pricePerM":1000000},{"item":"output","units":100,"pricePerM":2000000}],"charged":316,"freeze":400}"#
        );
        // Within the ledger's limit, whatever the model is called.
        assert!(json.len() < 512);
        assert_eq!(json, settlement.description().unwrap());
    }

    /// The description is the record a proof covers: the metered dimensions
    /// only, because attribution and the provider's raw report may carry caller
    /// or prompt data — they belong to the mutable usage row, not an immutable
    /// ledger entry (docs/decisions.md, data retention).
    #[test]
    fn the_description_drops_attribution_and_the_raw_report() {
        let mut usage = UsageRecord::tokens(11, 7).unwrap();
        usage.cached_tokens = 8;
        usage.end_user = Some("u_42".into());
        usage.service_tier = Some("priority".into());
        usage.tags.insert("team".into(), "search".into());
        usage.usage_details = Some(serde_json::json!({"provider_raw": {"x": 1}}));
        let json = Settlement {
            request: "abc",
            channel: "c",
            model: "m",
            price_version: 1,
            kind: SettlementKind::Usage,
            usage: &usage,
            input_price: 1,
            output_price: 1,
            charged: 1,
            freeze: 1,
        }
        .description()
        .unwrap();
        assert!(json.contains("\"cachedTokens\":8"), "{json}");
        assert!(json.contains("\"serviceTier\":\"priority\""), "{json}");
        for dropped in ["endUser", "tags", "usageDetails", "provider_raw"] {
            assert!(!json.contains(dropped), "{json} carries {dropped}");
        }
    }

    /// The reader and the writer are one shape: what `Settlement::description` writes is
    /// what `SettlementRecord::parse` reads, field for field. A field renamed on one side
    /// only would silently drop requests off the requests page (issue #55).
    #[test]
    fn a_settlement_description_reads_back() {
        let usage = UsageRecord::tokens(116, 100).unwrap();
        let settlement = Settlement {
            request: "abc",
            channel: "deepseek",
            model: "deepseek-chat",
            price_version: 3,
            kind: SettlementKind::Usage,
            usage: &usage,
            input_price: 1_000_000,
            output_price: 2_000_000,
            charged: 316,
            freeze: 400,
        };
        let record = SettlementRecord::parse(&settlement.description().unwrap()).unwrap();
        assert_eq!(record.request, "abc");
        assert_eq!(record.channel, "deepseek");
        assert_eq!(record.model, "deepseek-chat");
        assert_eq!(record.price_version, 3);
        assert_eq!(record.kind, SettlementKind::Usage);
        assert_eq!(record.input_tokens, 116);
        assert_eq!(record.output_tokens, 100);
        assert_eq!(record.input_price, 1_000_000);
        assert_eq!(record.output_price, 2_000_000);
        assert_eq!(record.charged, 316);
        assert_eq!(record.freeze, 400);
        assert_eq!(record.usage, MeteredUsage::from(&usage));
        assert_eq!(record.lines.len(), 2);

        // Every settlement kind round-trips: the page shows the record's own word.
        for kind in [
            SettlementKind::Estimated,
            SettlementKind::ClientCancelled,
            SettlementKind::UpstreamError,
            SettlementKind::UpstreamUnreachable,
            SettlementKind::Capped,
            SettlementKind::Swept,
        ] {
            let text = Settlement {
                kind,
                ..settlement.clone()
            }
            .description()
            .unwrap();
            assert_eq!(SettlementRecord::parse(&text).unwrap().kind, kind);
        }
    }

    /// A hold's description is not a settlement's, and neither is text that is not a record
    /// at all: the requests page must not read a freeze as a request.
    #[test]
    fn only_a_settlement_description_parses_as_a_record() {
        assert!(SettlementRecord::parse(&hold_description("abc", "m", 400).unwrap()).is_none());
        assert!(SettlementRecord::parse("").is_none());
        assert!(SettlementRecord::parse("not json").is_none());
        // A record missing the fields a request is billed by is not one either.
        assert!(SettlementRecord::parse(r#"{"request":"abc","model":"m"}"#).is_none());
        // The flat record from before descriptions were versioned is not this
        // build's to read: no `v`, no interpretation (docs/decisions.md, T1-2).
        assert!(SettlementRecord::parse(r#"{"request":"a","channel":"c","model":"m","priceVersion":1,"kind":"usage","inputTokens":1,"outputTokens":1,"inputPrice":1,"outputPrice":1,"charged":1,"freeze":1}"#).is_none());
        // Neither is one from the future, or one whose priced lines are missing.
        assert!(SettlementRecord::parse(r#"{"v":9,"request":"a","channel":"c","model":"m","priceVersion":1,"kind":"usage","usage":{"inputTokens":1},"lines":[{"item":"input","units":1,"pricePerM":1},{"item":"output","units":0,"pricePerM":1}],"charged":1,"freeze":1}"#).is_none());
        assert!(SettlementRecord::parse(r#"{"v":2,"request":"a","channel":"c","model":"m","priceVersion":1,"kind":"usage","usage":{"inputTokens":1},"lines":[],"charged":1,"freeze":1}"#).is_none());
        // An unknown settlement kind is not one this build can price.
        assert!(SettlementRecord::parse(r#"{"v":2,"request":"a","channel":"c","model":"m","priceVersion":1,"kind":"teleported","usage":{},"lines":[{"item":"input","units":0,"pricePerM":1},{"item":"output","units":0,"pricePerM":1}],"charged":0,"freeze":1}"#).is_none());
    }

    #[test]
    fn the_hold_description_names_the_request_and_the_freeze() {
        assert_eq!(
            hold_description("abc", "m", 400).unwrap(),
            r#"{"request":"abc","model":"m","freeze":400,"kind":"hold"}"#
        );
    }

    #[test]
    fn estimation_needs_no_network_and_stays_under_the_byte_bound() {
        let text = "The quick brown fox jumps over the lazy dog, twice.";
        let estimate = estimate_tokens(&[text]);
        assert!(estimate > 0);
        // A byte-level tokenizer cannot produce more tokens than bytes, so estimation stays inside
        // the input upper bound the freeze was computed from.
        assert!(estimate <= input_upper_bound(&[text]));
    }

    #[test]
    fn the_book_parses_a_configuration_and_refuses_bad_ones() {
        let book = PriceBook::from_json(
            "deepseek",
            r#"{"deepseek-chat":{"inputPricePerMillion":500000,"outputPricePerMillion":1500000,"maxOutputTokens":8192}}"#,
        )
        .unwrap();
        assert_eq!(book.channel(), "deepseek");
        assert_eq!(book.get("deepseek-chat").unwrap().max_output_tokens, 8192);
        assert!(book.get("gpt-4o").is_none());
        assert_eq!(book.models().count(), 1);

        assert!(PriceBook::from_json("c", "{}").is_err());
        assert!(PriceBook::from_json("c", "not json").is_err());
        // An unknown field is a typo in a price, and a typo in a price is a wrong bill.
        assert!(
            PriceBook::from_json(
                "c",
                r#"{"m":{"inputPricePerMillion":1,"outputPricePerMillion":1,"maxOutputTokens":1,"inputPrice":2}}"#
            )
            .is_err()
        );
        assert!(
            PriceBook::from_json(
                "c",
                r#"{"m":{"inputPricePerMillion":1,"outputPricePerMillion":1,"maxOutputTokens":0}}"#
            )
            .is_err()
        );
        assert!(
            PriceBook::from_json(
                "c",
                r#"{"m":{"inputPricePerMillion":-1,"outputPricePerMillion":1,"maxOutputTokens":1}}"#
            )
            .is_err()
        );
        assert!(
            PriceBook::from_json(
                "c",
                r#"{" ":{ "inputPricePerMillion":1,"outputPricePerMillion":1,"maxOutputTokens":1}}"#
            )
            .is_err()
        );
    }
}
