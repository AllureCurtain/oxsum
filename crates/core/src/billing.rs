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

/// The billing mode a price applies to: what the usage record's counts mean.
/// Only `chat` exists today; the field is versioned with the price so a new
/// mode is an addition, not a reinterpretation of an old row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BillingMode {
    /// A chat-completion turn, priced on token usage.
    #[default]
    Chat,
}

impl BillingMode {
    /// The spelling `channel_prices.mode` stores.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Chat => "chat",
        }
    }

    /// Reads a stored mode back; an unknown one is a price this build cannot
    /// apply, and the caller refuses it rather than guess.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "chat" => Some(Self::Chat),
            _ => None,
        }
    }
}

/// The upstream's side of the price: what the channel costs the deployment for
/// the same usage, in the same units as the customer price. Sparse — only the
/// dimensions upstream actually meters are written. The margin view and the
/// upstream-misbilling checks read it (roadmap P1-6); it never reaches an
/// organization's bill.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct UpstreamPrices {
    /// Minor units per million input tokens upstream charges.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_price_per_million: Option<i64>,
    /// Minor units per million output tokens upstream charges.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_price_per_million: Option<i64>,
    /// Minor units per million cached input tokens upstream charges.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_price_per_million: Option<i64>,
    /// Minor units per million tokens written into upstream's 5-minute cache tier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_5m_price_per_million: Option<i64>,
    /// Minor units per million tokens written into upstream's 1-hour cache tier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_1h_price_per_million: Option<i64>,
    /// Minor units per million reasoning tokens upstream charges.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_price_per_million: Option<i64>,
    /// A flat minor-unit amount upstream charges per request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_per_request: Option<i64>,
}

impl UpstreamPrices {
    /// The non-negativity check every price is under.
    fn validate(&self) -> Result<(), String> {
        for rate in [
            self.input_price_per_million,
            self.output_price_per_million,
            self.cache_read_price_per_million,
            self.cache_write_5m_price_per_million,
            self.cache_write_1h_price_per_million,
            self.reasoning_price_per_million,
            self.cost_per_request,
        ]
        .into_iter()
        .flatten()
        {
            if rate < 0 {
                return Err("upstream prices must be zero or more".into());
            }
        }
        Ok(())
    }
}

/// A whole price set: what a matched rule swaps in, and what the base price is
/// made of. The rates are minor units per million units; a dimension with no
/// rate is not free — it bills at its side's base rate and folds into the
/// `input` or `output` line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PriceSet {
    /// Minor units per million input tokens.
    pub input_price_per_million: i64,
    /// Minor units per million output tokens.
    pub output_price_per_million: i64,
    /// The most output the model may produce; the ceiling `max_tokens` clamps to.
    pub max_output_tokens: i64,
    /// Minor units per million cached input tokens, when the cached part prices
    /// differently from fresh input.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_price_per_million: Option<i64>,
    /// Minor units per million tokens written into the 5-minute provider cache tier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_5m_price_per_million: Option<i64>,
    /// Minor units per million tokens written into the 1-hour provider cache tier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_1h_price_per_million: Option<i64>,
    /// Minor units per million reasoning tokens, when reasoning prices
    /// differently from ordinary output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_price_per_million: Option<i64>,
    /// A flat amount in minor units, charged once per billed request on top of
    /// the token lines.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_per_request: Option<i64>,
    /// What upstream charges for the same usage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream: Option<UpstreamPrices>,
}

impl PriceSet {
    /// Checks the parts of a set that no arithmetic downstream can repair.
    fn validate(&self) -> Result<(), String> {
        for rate in [
            Some(self.input_price_per_million),
            Some(self.output_price_per_million),
            self.cache_read_price_per_million,
            self.cache_write_5m_price_per_million,
            self.cache_write_1h_price_per_million,
            self.reasoning_price_per_million,
        ]
        .into_iter()
        .flatten()
        {
            if rate < 0 {
                return Err("prices must be zero or more minor units per million units".into());
            }
        }
        if self.max_output_tokens <= 0 {
            return Err("maxOutputTokens must be positive".into());
        }
        // The flat fee rides a line whose rate is per million requests, so the
        // encode must still fit a minor unit count.
        if let Some(flat) = self.cost_per_request
            && !(0..=i64::MAX / PER_MILLION).contains(&flat)
        {
            return Err("costPerRequest must be a non-negative amount".into());
        }
        if let Some(upstream) = &self.upstream {
            upstream.validate()?;
        }
        Ok(())
    }

    /// The dearest rate a token on the input side can bill at: the base rate or
    /// a configured cache rate, whichever is largest. The freeze is only a
    /// promise when it covers the most expensive path.
    fn input_ceiling(&self) -> i64 {
        self.input_price_per_million
            .max(self.cache_read_price_per_million.unwrap_or(0))
            .max(self.cache_write_5m_price_per_million.unwrap_or(0))
            .max(self.cache_write_1h_price_per_million.unwrap_or(0))
    }

    /// As [`input_ceiling`](Self::input_ceiling), for the output side.
    fn output_ceiling(&self) -> i64 {
        self.output_price_per_million
            .max(self.reasoning_price_per_million.unwrap_or(0))
    }

    /// The lines a settlement decomposes into under this set: one per dimension
    /// the price actually prices, the rest folded into the side's base line —
    /// a cache dimension with no configured rate is billed at the input rate,
    /// which is what "no cache discount" means. `billable` is whether the
    /// request ran: a failed or swept turn owes no flat fee.
    fn lines(&self, usage: &UsageRecord, billable: bool) -> Vec<BillLine> {
        let mut input_units = usage.input_tokens;
        let mut lines = Vec::with_capacity(6);
        for (units, rate, item) in [
            (
                usage.cached_tokens,
                self.cache_read_price_per_million,
                "cache_read",
            ),
            (
                usage.cache_write_5m_tokens,
                self.cache_write_5m_price_per_million,
                "cache_write_5m",
            ),
            (
                usage.cache_write_1h_tokens,
                self.cache_write_1h_price_per_million,
                "cache_write_1h",
            ),
        ] {
            if let Some(rate) = rate.filter(|_| units > 0) {
                input_units -= units;
                lines.push(BillLine {
                    item: item.into(),
                    units,
                    price_per_m: rate,
                });
            }
        }
        let mut output_units = usage.output_tokens;
        let reasoning = (self.reasoning_price_per_million.is_some())
            .then_some(usage.reasoning_tokens)
            .filter(|units| *units > 0);
        if let Some(units) = reasoning {
            output_units -= units;
        }
        let mut result = Vec::with_capacity(lines.len() + 3);
        result.push(BillLine {
            item: "input".into(),
            units: input_units,
            price_per_m: self.input_price_per_million,
        });
        result.append(&mut lines);
        result.push(BillLine {
            item: "output".into(),
            units: output_units,
            price_per_m: self.output_price_per_million,
        });
        if let Some(units) = reasoning {
            result.push(BillLine {
                item: "reasoning".into(),
                units,
                price_per_m: self.reasoning_price_per_million.unwrap_or(0),
            });
        }
        if let Some(flat) = self.cost_per_request {
            result.push(BillLine {
                item: "request".into(),
                units: i64::from(billable),
                price_per_m: flat * PER_MILLION,
            });
        }
        result
    }

    /// What upstream's rates make of the same usage, in minor units — the
    /// platform's cost for the turn, beside what the customer was charged
    /// (roadmap P1-6, issue #112).
    ///
    /// The same convention the customer lines follow: a subset bills at its
    /// upstream rate when one is written, else folds into the side's upstream
    /// base rate; a side with no upstream base rate contributes nothing, and
    /// the upstream flat fee applies once on a billed turn. A set with no
    /// `upstream` block answers `None` — untracked, not zero, which is what
    /// `untrackedTurns` counts on the margin view. No freeze cap: the cap is a
    /// promise to the customer, not a ceiling on what upstream bills.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] when the total does not fit in 64 bits.
    pub fn upstream_minor(
        &self,
        usage: &UsageRecord,
        billable: bool,
    ) -> Result<Option<i64>, WalletError> {
        let Some(upstream) = &self.upstream else {
            return Ok(None);
        };
        let mut numerator: i128 = 0;
        let mut input_units = usage.input_tokens;
        for (units, rate) in [
            (usage.cached_tokens, upstream.cache_read_price_per_million),
            (
                usage.cache_write_5m_tokens,
                upstream.cache_write_5m_price_per_million,
            ),
            (
                usage.cache_write_1h_tokens,
                upstream.cache_write_1h_price_per_million,
            ),
        ] {
            if let Some(rate) = rate.filter(|_| units > 0) {
                input_units -= units;
                numerator += i128::from(units) * i128::from(rate);
            }
        }
        numerator +=
            i128::from(input_units) * i128::from(upstream.input_price_per_million.unwrap_or(0));
        let mut output_units = usage.output_tokens;
        if let Some(rate) = upstream
            .reasoning_price_per_million
            .filter(|_| usage.reasoning_tokens > 0)
        {
            output_units -= usage.reasoning_tokens;
            numerator += i128::from(usage.reasoning_tokens) * i128::from(rate);
        }
        numerator +=
            i128::from(output_units) * i128::from(upstream.output_price_per_million.unwrap_or(0));
        if let Some(flat) = upstream.cost_per_request.filter(|_| billable) {
            numerator += i128::from(flat) * i128::from(PER_MILLION);
        }
        // Ceiling division, by hand, like `ItemizedCharge::total_minor`.
        let per_million = i128::from(PER_MILLION);
        let minor = (numerator + per_million - 1) / per_million;
        i64::try_from(minor)
            .map(Some)
            .map_err(|_| WalletError::InvalidInput("the amount does not fit in 64 bits".into()))
    }
}

/// The conditions a request must satisfy for a rule's set to price it. Every
/// present field must hold; the count of present fields is the rule's
/// specificity, and the most specific matching rule wins.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct RuleMatch {
    /// Matches when the caller asked for this service tier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    /// Matches when the turn's input reaches this count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_input_tokens: Option<i64>,
    /// Matches when the turn's input stays under this count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_input_tokens: Option<i64>,
}

impl RuleMatch {
    /// The match's specificity: how many conditions it carries.
    fn specificity(&self) -> usize {
        usize::from(self.service_tier.is_some())
            + usize::from(self.min_input_tokens.is_some())
            + usize::from(self.max_input_tokens.is_some())
    }

    /// Checks the conditions themselves: at least one must exist — a rule that
    /// always matches is the base price wearing a costume — and a token window
    /// must not be empty.
    fn validate(&self) -> Result<(), String> {
        if self.specificity() == 0 {
            return Err("a price rule needs at least one condition".into());
        }
        if self.service_tier.as_deref().is_some_and(str::is_empty) {
            return Err("a rule's serviceTier cannot be empty".into());
        }
        if let (Some(min), Some(max)) = (self.min_input_tokens, self.max_input_tokens)
            && min > max
        {
            return Err("a rule's minInputTokens cannot exceed its maxInputTokens".into());
        }
        if self.min_input_tokens.is_some_and(|bound| bound < 0)
            || self.max_input_tokens.is_some_and(|bound| bound < 0)
        {
            return Err("a rule's token bounds cannot be negative".into());
        }
        Ok(())
    }

    /// Whether this usage satisfies every condition present.
    fn matches(&self, usage: &UsageRecord) -> bool {
        self.service_tier
            .as_ref()
            .is_none_or(|tier| usage.service_tier.as_ref() == Some(tier))
            && self
                .min_input_tokens
                .is_none_or(|bound| usage.input_tokens >= bound)
            && self
                .max_input_tokens
                .is_none_or(|bound| usage.input_tokens <= bound)
    }

    /// Whether a request asking for `service_tier` could match at all. The
    /// token bounds are not knowable before the turn runs, so they never
    /// disqualify — a freeze that prices the dearest possible tier must keep
    /// every rule they do not exclude.
    fn could_match(&self, service_tier: Option<&str>) -> bool {
        self.service_tier
            .as_deref()
            .is_none_or(|tier| Some(tier) == service_tier)
    }

    /// Whether some request could satisfy both matches at once: compatible
    /// tiers and intersecting token windows.
    fn overlaps(&self, other: &Self) -> bool {
        if self.service_tier.is_some()
            && other.service_tier.is_some()
            && self.service_tier != other.service_tier
        {
            return false;
        }
        let low = [self.min_input_tokens, other.min_input_tokens]
            .into_iter()
            .flatten()
            .max();
        let high = [self.max_input_tokens, other.max_input_tokens]
            .into_iter()
            .flatten()
            .min();
        low.zip(high).is_none_or(|(low, high)| low <= high)
    }
}

/// A conditional price: when the request matches, the rule's set replaces the
/// whole base set — never a field-level diff (docs/decisions.md, "Pricing").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PriceRule {
    /// The conditions, spelled `match` on the wire.
    #[serde(rename = "match")]
    pub cond: RuleMatch,
    /// The price set that takes over when `match` holds.
    pub price: PriceSet,
}

/// One model's price: the base set every request pays, the mode it applies to,
/// and the conditional rules that may swap the set in whole.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Price {
    /// Minor units per million input tokens. Zero is a model that is free to prompt.
    pub input_price_per_million: i64,
    /// Minor units per million output tokens.
    pub output_price_per_million: i64,
    /// The most output the model can produce. A request asking for more, or for nothing, gets this.
    pub max_output_tokens: i64,
    /// Minor units per million cached input tokens, when the cached part prices
    /// differently from fresh input.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_price_per_million: Option<i64>,
    /// Minor units per million tokens written into the 5-minute provider cache tier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_5m_price_per_million: Option<i64>,
    /// Minor units per million tokens written into the 1-hour provider cache tier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_1h_price_per_million: Option<i64>,
    /// Minor units per million reasoning tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_price_per_million: Option<i64>,
    /// A flat amount in minor units, charged once per billed request on top of
    /// the token lines.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_per_request: Option<i64>,
    /// What upstream charges for the same usage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream: Option<UpstreamPrices>,
    /// The billing mode; only `chat` is priced today.
    #[serde(default)]
    pub mode: BillingMode,
    /// The conditional price sets, resolved most-specific-wins.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<PriceRule>,
}

/// A turn's charge decomposed: the priced lines and the rule whose set priced
/// them. [`ItemizedCharge::total_minor`] is one ceiling over the lines'
/// combined numerator — one rounding per turn, not per line, so the user never
/// pays a fraction of a minor unit per line.
#[derive(Debug)]
pub struct ItemizedCharge {
    /// The lines the settlement description writes.
    pub lines: Vec<BillLine>,
    /// The match that chose the set, when a conditional rule priced the turn.
    pub matched_rule: Option<RuleMatch>,
}

impl ItemizedCharge {
    /// The lines' summed cost before the division — what `total_minor` and the
    /// discount variant divide once.
    fn numerator(&self) -> i128 {
        let mut numerator: i128 = 0;
        for line in &self.lines {
            numerator += i128::from(line.units) * i128::from(line.price_per_m);
        }
        numerator
    }

    /// The charge the lines sum to, ceiling-divided as one amount.
    ///
    /// # Errors
    ///
    /// Refuses a total that does not fit in 64 bits.
    pub fn total_minor(&self) -> Result<i64, WalletError> {
        Self::ceiling(self.numerator(), i128::from(PER_MILLION))
    }

    /// The charge after an organization discount (issue #158): the single most
    /// favorable applicable discount, taken off the priced sum — the numerator
    /// scaled by `100 - percent` before the one ceiling division, the same rule
    /// `verify_charge`'s v4 recompute applies. `percent` is 1..=100 — a
    /// `pricing_discounts` check constrains it — and 100 settles at zero.
    ///
    /// # Errors
    ///
    /// Refuses a percent outside 1..=100 or a total that does not fit in 64 bits.
    pub fn discounted_minor(&self, percent: i64) -> Result<i64, WalletError> {
        if !(1..=100).contains(&percent) {
            return Err(WalletError::InvalidInput(
                "a discount percent is 1..=100".into(),
            ));
        }
        Self::ceiling(
            self.numerator() * i128::from(100 - percent),
            i128::from(PER_MILLION) * 100,
        )
    }

    /// One ceiling division, by hand: both sides are non-negative, and
    /// `i128::div_ceil` is still unstable.
    fn ceiling(numerator: i128, denominator: i128) -> Result<i64, WalletError> {
        let minor = (numerator + denominator - 1) / denominator;
        i64::try_from(minor)
            .map_err(|_| WalletError::InvalidInput("the amount does not fit in 64 bits".into()))
    }
}

impl Price {
    /// The base set: the part of this price a matching rule replaces.
    fn set(&self) -> PriceSet {
        PriceSet {
            input_price_per_million: self.input_price_per_million,
            output_price_per_million: self.output_price_per_million,
            max_output_tokens: self.max_output_tokens,
            cache_read_price_per_million: self.cache_read_price_per_million,
            cache_write_5m_price_per_million: self.cache_write_5m_price_per_million,
            cache_write_1h_price_per_million: self.cache_write_1h_price_per_million,
            reasoning_price_per_million: self.reasoning_price_per_million,
            cost_per_request: self.cost_per_request,
            upstream: self.upstream.clone(),
        }
    }

    /// Checks the parts of a price that no arithmetic downstream can repair:
    /// the base set's sanity, every rule's match and set, and that no two rules
    /// could match the same request at the same specificity — an ambiguous
    /// price book is refused when it is written, not discovered at a bill.
    ///
    /// # Errors
    ///
    /// Names the field when a rate is negative, the output ceiling is not
    /// positive, a rule is malformed, or two rules overlap ambiguously.
    pub fn validate(&self) -> Result<(), String> {
        self.set().validate()?;
        for rule in &self.rules {
            rule.cond.validate()?;
            rule.price.validate()?;
        }
        for (index, rule) in self.rules.iter().enumerate() {
            for other in &self.rules[index + 1..] {
                if rule.cond.specificity() == other.cond.specificity()
                    && rule.cond.overlaps(&other.cond)
                {
                    return Err(
                        "two rules could match the same request at the same specificity".into(),
                    );
                }
            }
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

    /// The most this request could cost: the input and output upper bounds
    /// priced at the dearest rate any candidate set bills them at, plus the
    /// highest flat fee, rounded up to the minor unit.
    ///
    /// A candidate is the base set plus every rule the caller's declared
    /// `service_tier` does not already disqualify — token bounds cannot be
    /// known before the turn runs, so a rule they do not exclude stays a
    /// candidate and the freeze covers its price (docs/decisions.md: the hold
    /// prices the highest applicable tier).
    ///
    /// # Errors
    ///
    /// Refuses a `max_tokens` of zero or less, and an amount that does not fit in 64 bits.
    pub fn freeze_minor(
        &self,
        texts: &[&str],
        asked_output: Option<i64>,
        service_tier: Option<&str>,
    ) -> Result<i64, WalletError> {
        self.bound(input_upper_bound(texts), asked_output, service_tier)
    }

    /// The same upper bound for a declared token shape — what `estimate-price`
    /// answers without a request ever running.
    ///
    /// The arithmetic is `freeze_minor`'s own: the input count stands in for the
    /// text upper bound, so the estimate is exactly what the gateway would freeze
    /// for a request that spent this shape, and a real settle never exceeds it.
    ///
    /// # Errors
    ///
    /// Refuses a negative input, a non-positive or overflowing output, and an
    /// amount that does not fit in 64 bits.
    pub fn estimate_minor(
        &self,
        input_tokens: i64,
        asked_output: Option<i64>,
        service_tier: Option<&str>,
    ) -> Result<i64, WalletError> {
        if input_tokens < 0 {
            return Err(WalletError::InvalidInput(
                "inputTokens must be zero or more".into(),
            ));
        }
        self.bound(input_tokens, asked_output, service_tier)
    }

    /// Dearest-candidate arithmetic behind `freeze_minor` and `estimate_minor`:
    /// the dearest rate any still-applicable set bills each side at, the highest
    /// flat fee, and `asked` clamped to the widest output ceiling.
    fn bound(
        &self,
        input: i64,
        asked_output: Option<i64>,
        service_tier: Option<&str>,
    ) -> Result<i64, WalletError> {
        let base = self.set();
        let candidates = std::iter::once(&base).chain(
            self.rules
                .iter()
                .filter(|rule| rule.cond.could_match(service_tier))
                .map(|rule| &rule.price),
        );
        let (mut input_rate, mut output_rate, mut flat, mut ceiling) = (0_i64, 0_i64, 0_i64, 0_i64);
        for set in candidates {
            input_rate = input_rate.max(set.input_ceiling());
            output_rate = output_rate.max(set.output_ceiling());
            flat = flat.max(set.cost_per_request.unwrap_or(0));
            ceiling = ceiling.max(set.max_output_tokens);
        }
        let asked = asked_output.unwrap_or(ceiling);
        if asked <= 0 {
            return Err(WalletError::InvalidInput(
                "max_tokens must be positive".into(),
            ));
        }
        let output = asked.min(ceiling);
        // i128 so the multiplication cannot overflow before the division brings it back down.
        let numerator = i128::from(input) * i128::from(input_rate)
            + i128::from(output) * i128::from(output_rate)
            + i128::from(flat) * i128::from(PER_MILLION);
        let per_million = i128::from(PER_MILLION);
        let minor = (numerator + per_million - 1) / per_million;
        i64::try_from(minor)
            .map_err(|_| WalletError::InvalidInput("the amount does not fit in 64 bits".into()))
    }

    /// The set that bills this usage, and the rule that chose it — the most
    /// specific match, which [`validate`](Self::validate) guarantees is unique.
    fn resolve(&self, usage: &UsageRecord) -> (PriceSet, Option<RuleMatch>) {
        let best = self
            .rules
            .iter()
            .filter(|rule| rule.cond.matches(usage))
            .max_by_key(|rule| rule.cond.specificity());
        match best {
            Some(rule) => (rule.price.clone(), Some(rule.cond.clone())),
            None => (self.set(), None),
        }
    }

    /// The lines a settlement decomposes the charge into, under the set the
    /// usage matched, and the rule that chose it when one did.
    ///
    /// # Errors
    ///
    /// Refuses usage that violates its own invariants — a record the verifier
    /// could never confirm must not be written in the first place.
    pub fn itemize(
        &self,
        usage: &UsageRecord,
        billable: bool,
    ) -> Result<ItemizedCharge, WalletError> {
        usage.validate()?;
        let (set, rule) = self.resolve(usage);
        Ok(ItemizedCharge {
            lines: set.lines(usage, billable),
            matched_rule: rule,
        })
    }

    /// What upstream billed the platform for the same turn: the set the turn
    /// priced under resolves the same way — a matched rule's `upstream` block
    /// wins with it — and the usage prices at upstream's rates. `None` when
    /// that set carries no `upstream` block: untracked, not zero.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] when the usage is invalid or the total
    /// does not fit in 64 bits.
    pub fn upstream_cost(
        &self,
        usage: &UsageRecord,
        billable: bool,
    ) -> Result<Option<i64>, WalletError> {
        usage.validate()?;
        let (set, _) = self.resolve(usage);
        set.upstream_minor(usage, billable)
    }

    /// The metered dimensions no price set can cover, named: the counts that sit
    /// outside both sides' totals — tool calls and the media tokens — and a
    /// foreign event kind. A turn carrying any of them settles `unpriced`: the
    /// part the book *can* bill is charged and the rest is the platform's
    /// recorded loss, never zero billed nor folded into a rate the dimension
    /// does not belong to (fail-closed, docs/decisions.md).
    ///
    /// `serviceTier` is not here: rules may match on it, so it is priced input,
    /// not usage. `usage_details` is not either: it is the record's data escape
    /// hatch, not a metered dimension — an adapter that learns to read a new
    /// count names it a column, which is what makes it priceable or flagged.
    #[must_use]
    pub fn unpriced_dimensions(&self, usage: &UsageRecord) -> Vec<&'static str> {
        let mut unpriced = Vec::new();
        for (name, count) in [
            ("toolCalls", usage.tool_calls),
            ("imageInputTokens", usage.image_input_tokens),
            ("audioInputTokens", usage.audio_input_tokens),
            ("videoInputTokens", usage.video_input_tokens),
            ("imageOutputTokens", usage.image_output_tokens),
            ("audioOutputTokens", usage.audio_output_tokens),
        ] {
            if count > 0 {
                unpriced.push(name);
            }
        }
        if usage.event_type.is_some() {
            unpriced.push("eventType");
        }
        unpriced
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
    /// Upstream's usage named a dimension the price book cannot cover — a tool
    /// call, a media token, a foreign event kind. The computable part was
    /// charged, capped by the freeze; the unpriced rest is the platform's loss,
    /// flagged for the anomalies page rather than billed at zero or at a rate
    /// the dimension does not belong to (fail-closed, roadmap P1-5).
    Unpriced,
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
            Self::Unpriced => "unpriced",
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
/// The version the settlement description writes on the wire.
/// [`SettlementRecord::parse`] reads this and v3 — the shape it superseded,
/// which differs only by the absent `discountPercent`. A record that names
/// another one is not this build's to interpret: `oxsum_verify`'s
/// `verify_charge` dispatches on `v` and leaves those to inclusion proof alone
/// (docs/decisions.md, T1-2).
const DESCRIPTION_VERSION: i64 = 4;

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
    /// The priced lines the charge decomposes into — [`Price::itemize`]'s
    /// output: one per dimension the price prices, the rest folded into the
    /// side's base line.
    pub lines: &'a [BillLine],
    /// The match that chose the set, when a conditional rule priced the turn:
    /// the audit answer to "which rule hit", recorded under the hash.
    pub matched_rule: Option<&'a RuleMatch>,
    /// The discount the turn's organization qualified for (issue #158):
    /// snapshotted because the row may change or end after — the multiplier in
    /// D10's `price × multiplier`. Absent when none applied.
    pub discount_percent: Option<i64>,
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
/// the rule `oxsum_verify::verify_charge` recomputes (v3's `lines`).
///
/// On the wire a line is a three-element array, `[item, units, pricePerMillion]`
/// — the ledger's 512-character description limit cannot afford an object's
/// repeated keys, and the item name keeps the tuple self-describing.
#[derive(Debug, Clone, PartialEq)]
pub struct BillLine {
    /// The dimension this line prices (`"input"`, `"output"`, `"cache_read"`,
    /// `"reasoning"`, `"request"`, …).
    pub item: String,
    /// How much of it the turn used, in the item's own units.
    pub units: i64,
    /// Minor units per million units.
    pub price_per_m: i64,
}

impl Serialize for BillLine {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        (&self.item, self.units, self.price_per_m).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for BillLine {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let (item, units, price_per_m) = <(String, i64, i64)>::deserialize(deserializer)?;
        Ok(Self {
            item,
            units,
            price_per_m,
        })
    }
}

/// A settlement's description on the wire, versioned.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SettlementV4<'a> {
    /// The schema version — [`DESCRIPTION_VERSION`]. Spelled `v` on the wire.
    v: i64,
    request: &'a str,
    channel: &'a str,
    model: &'a str,
    price_version: i64,
    kind: SettlementKind,
    usage: MeteredUsage,
    lines: &'a [BillLine],
    #[serde(skip_serializing_if = "Option::is_none")]
    matched_rule: Option<&'a RuleMatch>,
    /// The multiplier the priced sum was scaled by, v4's addition: the applied
    /// discount's percent, absent when none applied.
    #[serde(skip_serializing_if = "Option::is_none")]
    discount_percent: Option<i64>,
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
        serde_json::to_string(&SettlementV4 {
            v: DESCRIPTION_VERSION,
            request: self.request,
            channel: self.channel,
            model: self.model,
            price_version: self.price_version,
            kind: self.kind,
            usage: MeteredUsage::from(self.usage),
            lines: self.lines,
            matched_rule: self.matched_rule,
            discount_percent: self.discount_percent,
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
    /// The rule that priced the turn, when one did.
    pub matched_rule: Option<RuleMatch>,
    /// The discount the settlement applied (v4); `None` when none did — and for
    /// a v3 record, which never carried one.
    pub discount_percent: Option<i64>,
}

/// The wire [`SettlementRecord::parse`] reads: [`SettlementV4`] owned, tolerant
/// of v3 — its only difference is the `discountPercent` it never carried.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettlementRecordWire {
    v: i64,
    request: String,
    channel: String,
    model: String,
    price_version: i64,
    kind: SettlementKind,
    usage: MeteredUsage,
    lines: Vec<BillLine>,
    matched_rule: Option<RuleMatch>,
    discount_percent: Option<i64>,
    charged: i64,
    freeze: i64,
}

impl SettlementRecord {
    /// The record one entry's description carries, or `None` when the description is not a
    /// settlement's — a hold's record, an empty description, a version this build does not
    /// write, or text from a writer that is not this one.
    #[must_use]
    pub fn parse(description: &str) -> Option<Self> {
        let wire: SettlementRecordWire = serde_json::from_str(description).ok()?;
        if !(3..=DESCRIPTION_VERSION).contains(&wire.v) {
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
            matched_rule: wire.matched_rule,
            discount_percent: wire.discount_percent,
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

    /// One credit per million input tokens, two per million output, eight
    /// thousand tokens of output.
    fn price() -> Price {
        Price {
            input_price_per_million: 1_000_000,
            output_price_per_million: 2_000_000,
            max_output_tokens: 8_000,
            cache_read_price_per_million: None,
            cache_write_5m_price_per_million: None,
            cache_write_1h_price_per_million: None,
            reasoning_price_per_million: None,
            cost_per_request: None,
            upstream: None,
            mode: BillingMode::Chat,
            rules: Vec::new(),
        }
    }

    /// The two lines an ordinary turn decomposes into.
    fn lines(input: i64, output: i64) -> Vec<BillLine> {
        vec![
            BillLine {
                item: "input".into(),
                units: input,
                price_per_m: 1_000_000,
            },
            BillLine {
                item: "output".into(),
                units: output,
                price_per_m: 2_000_000,
            },
        ]
    }

    /// The itemized charge for a usage: the lines and the matched rule together.
    fn itemize(price: &Price, usage: &UsageRecord) -> ItemizedCharge {
        price.itemize(usage, true).unwrap()
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
            .freeze_minor(&["x".repeat(84).as_str()], Some(100), None)
            .unwrap();
        assert_eq!(freeze, 100 + 200);
    }

    #[test]
    fn a_fraction_of_a_minor_unit_rounds_up() {
        let cheap = Price {
            input_price_per_million: 1,
            output_price_per_million: 0,
            max_output_tokens: 1,
            ..price()
        };
        // An empty message is still the per-message overhead in input tokens, which at one minor
        // unit per million tokens is a millionth of a minor unit — and that still rounds up to one.
        assert_eq!(cheap.freeze_minor(&[""], None, None).unwrap(), 1);
    }

    #[test]
    fn the_estimate_is_the_freeze_arithmetic_on_a_declared_shape() {
        // 100 declared input tokens at 1 credit/mtok + 100 output at 2 credits/mtok,
        // the same number freeze_minor would compute for a request whose text bound
        // came to 100 input tokens.
        let price = price();
        assert_eq!(price.estimate_minor(100, Some(100), None).unwrap(), 300);
        assert_eq!(
            price.estimate_minor(100, Some(100), None).unwrap(),
            price.bound(100, Some(100), None).unwrap()
        );
        // The output clamps to the ceiling exactly like the freeze.
        assert_eq!(
            price.estimate_minor(0, Some(1_000_000), None).unwrap(),
            price.estimate_minor(0, Some(8_000), None).unwrap()
        );
        // A negative input is refused, not zero-priced.
        assert!(price.estimate_minor(-1, Some(1), None).is_err());
        assert!(price.estimate_minor(0, Some(0), None).is_err());
    }

    #[test]
    fn usage_is_priced_with_the_same_rounding() {
        let usage = UsageRecord::tokens(1_000_000, 500_000).unwrap();
        assert_eq!(
            itemize(&price(), &usage).total_minor().unwrap(),
            1_000_000 + 1_000_000
        );
        assert!(UsageRecord::tokens(-1, 0).is_err());
        // A price high enough to overflow i64 once multiplied is refused, not wrapped.
        let absurd = Price {
            input_price_per_million: i64::MAX,
            output_price_per_million: i64::MAX,
            max_output_tokens: 1,
            ..price()
        };
        assert!(
            itemize(&absurd, &UsageRecord::tokens(i64::MAX, i64::MAX).unwrap())
                .total_minor()
                .is_err()
        );
    }

    #[test]
    fn a_priced_cache_dimension_gets_its_own_line() {
        let priced = Price {
            cache_read_price_per_million: Some(100_000),
            reasoning_price_per_million: Some(8_000_000),
            ..price()
        };
        let mut usage = UsageRecord::tokens(100, 50).unwrap();
        usage.cached_tokens = 80;
        usage.reasoning_tokens = 20;
        let charge = itemize(&priced, &usage);
        // Input minus the cached part at the base rate, the cache at its own,
        // output minus reasoning at the base rate, reasoning at its own.
        assert_eq!(
            charge
                .lines
                .iter()
                .map(|line| (line.item.as_str(), line.units, line.price_per_m))
                .collect::<Vec<_>>(),
            vec![
                ("input", 20, 1_000_000),
                ("cache_read", 80, 100_000),
                ("output", 30, 2_000_000),
                ("reasoning", 20, 8_000_000),
            ]
        );
        // ceil((20·1e6 + 80·1e5 + 30·2e6 + 20·8e6)/1e6) = 20+8+60+160.
        assert_eq!(charge.total_minor().unwrap(), 248);
    }

    #[test]
    fn an_unpriced_dimension_folds_into_the_base_line() {
        // No cache price configured: the cached part bills at the input rate —
        // one `input` line for the whole count.
        let mut usage = UsageRecord::tokens(100, 50).unwrap();
        usage.cached_tokens = 80;
        usage.reasoning_tokens = 20;
        let charge = itemize(&price(), &usage);
        assert_eq!(charge.lines.len(), 2);
        assert_eq!(charge.lines[0].units, 100);
        assert_eq!(charge.lines[1].units, 50);
    }

    #[test]
    fn the_flat_fee_joins_the_lines_only_when_billed() {
        let flat = Price {
            cost_per_request: Some(500),
            ..price()
        };
        let usage = UsageRecord::tokens(0, 0).unwrap();
        let billed = flat.itemize(&usage, true).unwrap();
        // units 1 at the per-million-encoded flat rate: exactly `costPerRequest`.
        assert_eq!(billed.lines.last().unwrap().item, "request");
        assert_eq!(billed.lines.last().unwrap().units, 1);
        assert_eq!(billed.total_minor().unwrap(), 500);
        // A turn upstream never served owes nothing, and the record still says why.
        let not_billed = flat.itemize(&usage, false).unwrap();
        assert_eq!(not_billed.lines.last().unwrap().units, 0);
        assert_eq!(not_billed.total_minor().unwrap(), 0);
    }

    /// The fail-closed check names every metered dimension the book cannot
    /// cover, and only those (issue #110).
    #[test]
    fn unpriced_dimensions_are_named_never_billed_silently() {
        // Text usage is fully covered — including the cache and reasoning
        // counts, which are dimensions the book knows whether or not a rate
        // is configured for them.
        let mut usage = UsageRecord::tokens(100, 50).unwrap();
        usage.cached_tokens = 40;
        usage.reasoning_tokens = 10;
        usage.service_tier = Some("priority".into());
        assert!(price().unpriced_dimensions(&usage).is_empty());

        // A tool call, the media counts, a foreign event kind: each is named.
        let mut usage = UsageRecord::tokens(100, 50).unwrap();
        usage.tool_calls = 2;
        usage.audio_input_tokens = 300;
        usage.video_input_tokens = 7;
        usage.event_type = Some("response.created".into());
        assert_eq!(
            price().unpriced_dimensions(&usage),
            vec![
                "toolCalls",
                "audioInputTokens",
                "videoInputTokens",
                "eventType"
            ]
        );

        // A zero count is absent usage, not an unpriced dimension — and while a
        // dimension is flagged, the part the book covers still prices exactly.
        let mut usage = UsageRecord::tokens(100, 50).unwrap();
        usage.image_output_tokens = 4;
        assert_eq!(
            price().unpriced_dimensions(&usage),
            vec!["imageOutputTokens"]
        );
        assert_eq!(itemize(&price(), &usage).total_minor().unwrap(), 100 + 100);
    }

    /// The rules: a matched condition swaps the whole set, most specific wins,
    /// and the charge's lines carry the swap's prices — which is what makes the
    /// record recompute from its own fields alone.
    #[test]
    fn the_most_specific_matching_rule_prices_the_turn() {
        let mut ruled = price();
        ruled.rules = vec![
            // The base 128k-and-up tier: input at half price.
            PriceRule {
                cond: RuleMatch {
                    min_input_tokens: Some(128_000),
                    ..RuleMatch::default()
                },
                price: PriceSet {
                    input_price_per_million: 500_000,
                    ..price().set()
                },
            },
            // The same tier on the priority lane: a different input rate still.
            PriceRule {
                cond: RuleMatch {
                    service_tier: Some("priority".into()),
                    min_input_tokens: Some(128_000),
                    ..RuleMatch::default()
                },
                price: PriceSet {
                    input_price_per_million: 250_000,
                    ..price().set()
                },
            },
        ];
        ruled.validate().unwrap();

        let mut long = UsageRecord::tokens(200_000, 10).unwrap();
        let charge = itemize(&ruled, &long);
        // One condition matched: the token tier.
        assert_eq!(charge.lines[0].price_per_m, 500_000);
        assert_eq!(
            charge.matched_rule,
            Some(RuleMatch {
                min_input_tokens: Some(128_000),
                ..RuleMatch::default()
            })
        );

        // Two conditions matched on the priority lane: the more specific rule wins.
        long.service_tier = Some("priority".into());
        let charge = itemize(&ruled, &long);
        assert_eq!(charge.lines[0].price_per_m, 250_000);
        assert_eq!(
            charge.matched_rule.unwrap().service_tier.as_deref(),
            Some("priority")
        );

        // Nothing matched: the base set, no rule named.
        let short = UsageRecord::tokens(100, 10).unwrap();
        let charge = itemize(&ruled, &short);
        assert_eq!(charge.lines[0].price_per_m, 1_000_000);
        assert_eq!(charge.matched_rule, None);
    }

    /// `upstream` set to the base rates the channel bills the platform at —
    /// half the customer price here (issue #112).
    fn upstream() -> UpstreamPrices {
        UpstreamPrices {
            input_price_per_million: Some(500_000),
            output_price_per_million: Some(1_000_000),
            ..UpstreamPrices::default()
        }
    }

    #[test]
    fn upstream_cost_prices_the_same_usage_at_upstream_rates() {
        let mut tracked = price();
        tracked.upstream = Some(upstream());
        let usage = UsageRecord::tokens(1_000_000, 500_000).unwrap();
        assert_eq!(
            tracked.upstream_cost(&usage, true).unwrap(),
            Some(1_000_000)
        );
        // A price with no `upstream` block is untracked: `None`, not zero —
        // the margin view counts it separately, because zero would hide a
        // coverage gap inside a real cost.
        assert_eq!(price().upstream_cost(&usage, true).unwrap(), None);
    }

    #[test]
    fn upstream_cost_follows_the_matched_rules_set() {
        let mut ruled = price();
        ruled.upstream = Some(upstream());
        ruled.rules = vec![PriceRule {
            cond: RuleMatch {
                service_tier: Some("priority".into()),
                ..RuleMatch::default()
            },
            price: PriceSet {
                // The priority lane costs upstream more.
                upstream: Some(UpstreamPrices {
                    input_price_per_million: Some(2_000_000),
                    output_price_per_million: Some(4_000_000),
                    ..UpstreamPrices::default()
                }),
                ..price().set()
            },
        }];
        ruled.validate().unwrap();
        let mut usage = UsageRecord::tokens(1_000_000, 0).unwrap();
        assert_eq!(ruled.upstream_cost(&usage, true).unwrap(), Some(500_000));
        usage.service_tier = Some("priority".into());
        // The rule's set replaced the base — its `upstream` block came with it.
        assert_eq!(ruled.upstream_cost(&usage, true).unwrap(), Some(2_000_000));
    }

    #[test]
    fn upstream_cost_separates_what_it_prices_and_folds_the_rest() {
        let mut tracked = price();
        tracked.upstream = Some(UpstreamPrices {
            cache_read_price_per_million: Some(50_000),
            reasoning_price_per_million: Some(4_000_000),
            cost_per_request: Some(400),
            ..upstream()
        });
        let mut usage = UsageRecord::tokens(100, 50).unwrap();
        usage.cached_tokens = 80;
        usage.cache_write_5m_tokens = 20;
        usage.reasoning_tokens = 10;
        // Only a dimension upstream prices leaves the base rate: the 80 cached
        // tokens bill at their own rate, the 20 cache writes fold back into
        // input. 20·0.5 + 80·0.05 + 40·1 + 10·4 + 400 flat = 10+4+40+40+400.
        assert_eq!(tracked.upstream_cost(&usage, true).unwrap(), Some(494));
        // An unbilled turn owes no flat fee.
        let unpaid = UsageRecord::tokens(0, 0).unwrap();
        let flat_only = Price {
            upstream: Some(UpstreamPrices {
                cost_per_request: Some(400),
                ..UpstreamPrices::default()
            }),
            ..price()
        };
        assert_eq!(flat_only.upstream_cost(&unpaid, false).unwrap(), Some(0));
        // Upstream's arithmetic is the same ceiling-over-numerator, and the
        // same overflow refusal — never a wrap.
        let absurd = Price {
            upstream: Some(UpstreamPrices {
                input_price_per_million: Some(i64::MAX),
                ..UpstreamPrices::default()
            }),
            ..price()
        };
        assert!(
            absurd
                .upstream_cost(&UsageRecord::tokens(i64::MAX, 0).unwrap(), true)
                .is_err()
        );
    }

    #[test]
    fn an_ambiguous_rule_book_is_refused_when_written() {
        let mut ruled = price();
        ruled.rules = vec![
            // 64k–256k at one specificity…
            PriceRule {
                cond: RuleMatch {
                    min_input_tokens: Some(64_000),
                    max_input_tokens: Some(256_000),
                    ..RuleMatch::default()
                },
                price: price().set(),
            },
            // …and 128k and up at the same specificity: 128k–256k matches both.
            PriceRule {
                cond: RuleMatch {
                    min_input_tokens: Some(128_000),
                    max_input_tokens: Some(512_000),
                    ..RuleMatch::default()
                },
                price: price().set(),
            },
        ];
        assert!(ruled.validate().is_err());

        // A disjoint window does not overlap: different tiers never collide.
        ruled.rules[0].cond.service_tier = Some("batch".into());
        ruled.validate().unwrap();

        // A rule with no conditions, and one with an empty window, are malformed.
        ruled.rules[1].cond = RuleMatch::default();
        assert!(ruled.validate().is_err());
        ruled.rules[1].cond = RuleMatch {
            min_input_tokens: Some(200),
            max_input_tokens: Some(100),
            ..RuleMatch::default()
        };
        assert!(ruled.validate().is_err());
    }

    #[test]
    fn the_freeze_prices_the_dearest_rule_that_could_match() {
        let mut ruled = price();
        ruled.rules = vec![
            // A dearer cache-write rate the caller cannot rule out: input bounds
            // stay unknowable until the turn runs.
            PriceRule {
                cond: RuleMatch {
                    min_input_tokens: Some(1),
                    ..RuleMatch::default()
                },
                price: PriceSet {
                    cache_write_5m_price_per_million: Some(4_000_000),
                    ..price().set()
                },
            },
            // A priority-lane rule at an even dearer input rate, made more
            // specific than the window rule so the two can coexist: a
            // standard-lane caller does not freeze for it, a priority caller
            // does.
            PriceRule {
                cond: RuleMatch {
                    service_tier: Some("priority".into()),
                    min_input_tokens: Some(1),
                    ..RuleMatch::default()
                },
                price: PriceSet {
                    input_price_per_million: 5_000_000,
                    ..price().set()
                },
            },
        ];
        ruled.validate().unwrap();
        let text = "x".repeat(84);
        let texts = [text.as_str()];
        // Standard lane: input at the dearest possibly-matching rate — the
        // cache-write 4e6 beats the base 1e6.
        let freeze = ruled.freeze_minor(&texts, Some(100), None).unwrap();
        assert_eq!(freeze, 100 * 4 + 100 * 2);
        // Priority lane: the tier's 5e6 input is now the dearest candidate.
        let freeze = ruled
            .freeze_minor(&texts, Some(100), Some("priority"))
            .unwrap();
        assert_eq!(freeze, 100 * 5 + 100 * 2);
    }

    #[test]
    fn the_description_carries_the_arithmetic() {
        let usage = UsageRecord::tokens(116, 100).unwrap();
        let lines = lines(116, 100);
        let settlement = Settlement {
            request: "abc",
            channel: "deepseek",
            model: "deepseek-chat",
            price_version: 3,
            kind: SettlementKind::Usage,
            usage: &usage,
            lines: &lines,
            matched_rule: None,
            discount_percent: None,
            charged: 316,
            freeze: 400,
        };
        let json = settlement.description().unwrap();
        assert_eq!(
            json,
            r#"{"v":4,"request":"abc","channel":"deepseek","model":"deepseek-chat","priceVersion":3,"kind":"usage","usage":{"inputTokens":116,"outputTokens":100},"lines":[["input",116,1000000],["output",100,2000000]],"charged":316,"freeze":400}"#
        );
        // Within the ledger's limit, whatever the model is called.
        assert!(json.len() < 512);
        assert_eq!(json, settlement.description().unwrap());
    }

    /// A rule hit lands in the record: `matchedRule` is the match itself, so
    /// the bill names its tier without the reader needing the price history.
    #[test]
    fn a_matched_rule_is_named_in_the_description() {
        let usage = UsageRecord::tokens(116, 100).unwrap();
        let lines = lines(116, 100);
        let matched = RuleMatch {
            min_input_tokens: Some(100),
            ..RuleMatch::default()
        };
        let json = Settlement {
            request: "abc",
            channel: "c",
            model: "m",
            price_version: 3,
            kind: SettlementKind::Usage,
            usage: &usage,
            lines: &lines,
            matched_rule: Some(&matched),
            discount_percent: None,
            charged: 316,
            freeze: 400,
        }
        .description()
        .unwrap();
        assert!(
            json.contains(r#""matchedRule":{"minInputTokens":100}"#),
            "{json}"
        );
        let record = SettlementRecord::parse(&json).unwrap();
        assert_eq!(record.matched_rule, Some(matched));
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
        let lines = [
            BillLine {
                item: "input".into(),
                units: 3,
                price_per_m: 1,
            },
            BillLine {
                item: "output".into(),
                units: 7,
                price_per_m: 1,
            },
        ];
        let json = Settlement {
            request: "abc",
            channel: "c",
            model: "m",
            price_version: 1,
            kind: SettlementKind::Usage,
            usage: &usage,
            lines: &lines,
            matched_rule: None,
            discount_percent: None,
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
        let lines = lines(116, 100);
        let settlement = Settlement {
            request: "abc",
            channel: "deepseek",
            model: "deepseek-chat",
            price_version: 3,
            kind: SettlementKind::Usage,
            usage: &usage,
            lines: &lines,
            matched_rule: None,
            discount_percent: None,
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
            SettlementKind::Unpriced,
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
        // Neither is one from the future, one written before the itemized book
        // (v2), or one whose priced lines are missing. A v3 record parses — the
        // reader tolerates the shape v4 superseded — but one that lacks the
        // lines the prices are read out of still cannot.
        assert!(SettlementRecord::parse(r#"{"v":9,"request":"a","channel":"c","model":"m","priceVersion":1,"kind":"usage","usage":{"inputTokens":1},"lines":[["input",1,1],["output",0,1]],"charged":1,"freeze":1}"#).is_none());
        assert!(SettlementRecord::parse(r#"{"v":2,"request":"a","channel":"c","model":"m","priceVersion":1,"kind":"usage","usage":{"inputTokens":1},"lines":[{"item":"input","units":1,"pricePerM":1},{"item":"output","units":1,"pricePerM":1}],"charged":1,"freeze":1}"#).is_none());
        assert!(SettlementRecord::parse(r#"{"v":3,"request":"a","channel":"c","model":"m","priceVersion":1,"kind":"usage","usage":{"inputTokens":1},"lines":[],"charged":1,"freeze":1}"#).is_none());
        assert!(
            SettlementRecord::parse(r#"{"v":3,"request":"a","channel":"c","model":"m","priceVersion":1,"kind":"usage","usage":{"inputTokens":1},"lines":[["input",1,1],["output",0,1]],"charged":1,"freeze":1}"#).is_some(),
            "v3 descriptions still read back"
        );
        // An unknown settlement kind is not one this build can price.
        assert!(SettlementRecord::parse(r#"{"v":3,"request":"a","channel":"c","model":"m","priceVersion":1,"kind":"teleported","usage":{},"lines":[["input",0,1],["output",0,1]],"charged":0,"freeze":1}"#).is_none());
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
