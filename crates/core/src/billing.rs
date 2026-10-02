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
    pub fn cost_minor(&self, usage: Usage) -> Result<i64, WalletError> {
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

/// A token count: what upstream reported, or what a local estimate stands in for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Prompt tokens, as upstream counted them.
    pub input_tokens: i64,
    /// Completion tokens, as upstream counted them.
    pub output_tokens: i64,
}

impl Usage {
    /// # Errors
    ///
    /// Refuses a negative count: upstream reporting one is upstream being wrong, and pricing it would
    /// turn that into a refund.
    pub fn new(input_tokens: i64, output_tokens: i64) -> Result<Self, WalletError> {
        if input_tokens < 0 || output_tokens < 0 {
            return Err(WalletError::InvalidInput(
                "token counts cannot be negative".into(),
            ));
        }
        Ok(Self {
            input_tokens,
            output_tokens,
        })
    }
}

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
    /// Tokens billed as input.
    pub input_tokens: i64,
    /// Tokens billed as output.
    pub output_tokens: i64,
    /// Minor units per million input tokens at the time of the request.
    pub input_price: i64,
    /// Minor units per million output tokens at the time of the request.
    pub output_price: i64,
    /// What was charged, never more than `freeze`.
    pub charged: i64,
    /// What was frozen before the call.
    pub freeze: i64,
}

impl Settlement<'_> {
    /// The compact JSON the settlement entry's description carries.
    ///
    /// # Errors
    ///
    /// Refuses a description that cannot be serialised, which cannot happen for these fields; the
    /// `Result` keeps the caller honest rather than unwrapping a formality.
    pub fn description(&self) -> Result<String, WalletError> {
        serde_json::to_string(self)
            .map_err(|error| WalletError::InvalidInput(format!("settlement record: {error}")))
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
        let usage = Usage::new(1_000_000, 500_000).unwrap();
        assert_eq!(price().cost_minor(usage).unwrap(), 1_000_000 + 1_000_000);
        assert!(Usage::new(-1, 0).is_err());
        // A price high enough to overflow i64 once multiplied is refused, not wrapped.
        let absurd = Price {
            input_per_million: i64::MAX,
            output_per_million: i64::MAX,
            max_output_tokens: 1,
        };
        assert!(
            absurd
                .cost_minor(Usage::new(i64::MAX, i64::MAX).unwrap())
                .is_err()
        );
    }

    #[test]
    fn the_description_carries_the_arithmetic() {
        let settlement = Settlement {
            request: "abc",
            channel: "deepseek",
            model: "deepseek-chat",
            price_version: 3,
            kind: SettlementKind::Usage,
            input_tokens: 116,
            output_tokens: 100,
            input_price: 1_000_000,
            output_price: 2_000_000,
            charged: 316,
            freeze: 400,
        };
        let json = settlement.description().unwrap();
        assert_eq!(
            json,
            r#"{"request":"abc","channel":"deepseek","model":"deepseek-chat","priceVersion":3,"kind":"usage","inputTokens":116,"outputTokens":100,"inputPrice":1000000,"outputPrice":2000000,"charged":316,"freeze":400}"#
        );
        // Within the ledger's limit, whatever the model is called.
        assert!(json.len() < 512);
        assert_eq!(json, settlement.description().unwrap());
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
