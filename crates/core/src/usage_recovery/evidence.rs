//! Strict raw component presence and conservative independent component pricing.
use crate::{ItemizedCharge, Price, UsageRecord, WalletError};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Allowlisted observations; absent and invalid quantities are never observed zeroes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ObservedTextUsage {
    /// Total input, including cached tokens.
    pub input: Option<i64>,
    /// Total output, including reasoning tokens.
    pub output: Option<i64>,
    /// Explicit total retained as a cross-report constraint, never used to infer a side.
    pub total: Option<i64>,
    /// Explicit cached subset.
    pub cached: Option<i64>,
    /// Explicit cache-miss constraint; never used to derive an absent cached subset.
    pub cache_miss: Option<i64>,
    /// Explicit reasoning subset.
    pub reasoning: Option<i64>,
    /// Rejected component names, without raw provider content.
    pub invalid: Vec<String>,
    /// A single plain text completion was observed.
    pub eligible: bool,
    /// Provider completion identity, never an HTTP request ID.
    pub completion_id: Option<String>,
}
fn quantity(value: Option<&Value>, name: &str, invalid: &mut Vec<String>) -> Option<i64> {
    value.and_then(
        |v| match v.as_i64().filter(|n| (0..=1_000_000_000).contains(n)) {
            Some(n) => Some(n),
            None => {
                invalid.push(name.into());
                None
            }
        },
    )
}
impl ObservedTextUsage {
    /// Combine cumulative reports without summing; conflicting components stay unresolved.
    pub fn merge(&mut self, other: &Self) {
        fn combine(old: &mut Option<i64>, new: Option<i64>, name: &str, invalid: &mut Vec<String>) {
            let conflict = format!("conflicting_{name}");
            if invalid.contains(&conflict) {
                *old = None;
                return;
            }
            match (*old, new) {
                (Some(a), Some(b)) if a != b => {
                    *old = None;
                    invalid.push(conflict);
                }
                (None, Some(b)) => *old = Some(b),
                _ => {}
            }
        }
        if self
            .invalid
            .iter()
            .any(|i| i == "unsupported_recovered_completion_shape")
        {
            return;
        }
        combine(&mut self.input, other.input, "input", &mut self.invalid);
        combine(&mut self.output, other.output, "output", &mut self.invalid);
        combine(&mut self.total, other.total, "total", &mut self.invalid);
        combine(&mut self.cached, other.cached, "cached", &mut self.invalid);
        combine(
            &mut self.cache_miss,
            other.cache_miss,
            "cache_miss",
            &mut self.invalid,
        );
        combine(
            &mut self.reasoning,
            other.reasoning,
            "reasoning",
            &mut self.invalid,
        );
        for issue in &other.invalid {
            if !self.invalid.contains(issue) {
                self.invalid.push(issue.clone());
            }
        }
        // Invalid values are evidence too: a later malformed report must not
        // leave an earlier chargeable quantity behind or rehabilitate a conflict.
        self.reject_invalid_components();
        if self.invalid.iter().any(|s| s == "conflicting_total")
            || self.total.is_some_and(|t| {
                self.input.is_some_and(|i| i > t)
                    || self.output.is_some_and(|o| o > t)
                    || self
                        .input
                        .zip(self.output)
                        .is_some_and(|(i, o)| i.checked_add(o) != Some(t))
            })
        {
            self.input = None;
            self.output = None;
            self.invalid.push("conflicting_input".into());
            self.invalid.push("conflicting_output".into());
        }
        if self.cache_miss.zip(self.input).is_some_and(|(m, i)| m > i)
            || self
                .cache_miss
                .zip(self.cached)
                .zip(self.input)
                .is_some_and(|((m, c), i)| m.checked_add(c) != Some(i))
        {
            self.input = None;
            self.cached = None;
            self.invalid.push("invalid_cache_breakdown".into());
        }
        if self.cached.zip(self.input).is_some_and(|(c, i)| c > i) {
            self.input = None;
            self.cached = None;
            self.invalid.push("conflicting_input".into());
        }
        if self.reasoning.zip(self.output).is_some_and(|(r, o)| r > o) {
            self.output = None;
            self.reasoning = None;
            self.invalid.push("conflicting_output".into());
        }
        self.invalid.sort_unstable();
        self.invalid.dedup();
    }
    fn reject_invalid_components(&mut self) {
        for issue in &self.invalid {
            match issue.as_str() {
                "total" | "conflicting_total" | "contradictory_total" => {
                    self.total = None;
                    self.input = None;
                    self.output = None;
                }
                "input"
                | "conflicting_input"
                | "unsupported_input_details"
                | "cached_exceeds_input"
                | "invalid_cache_breakdown"
                | "conflicting_cache_miss" => {
                    self.input = None;
                    self.cached = None;
                }
                "output"
                | "conflicting_output"
                | "unsupported_output_details"
                | "reasoning_exceeds_output" => {
                    self.output = None;
                    self.reasoning = None;
                }
                "cached" | "conflicting_cached" => self.cached = None,
                "reasoning" | "conflicting_reasoning" => self.reasoning = None,
                _ => {}
            }
        }
    }
    /// Parse one complete nonstream response. Never derive or clamp provider counts.
    #[must_use]
    pub fn parse(value: &Value) -> Self {
        let completion_id = value
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| {
                !id.is_empty()
                    && id.len() <= 200
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            })
            .map(str::to_owned);
        let eligible = value.get("object").and_then(Value::as_str) == Some("chat.completion")
            && value
                .get("choices")
                .and_then(Value::as_array)
                .is_some_and(|c| {
                    c.len() == 1
                        && c[0].get("index").and_then(Value::as_i64) == Some(0)
                        && matches!(
                            c[0].get("finish_reason").and_then(Value::as_str),
                            Some("stop" | "length" | "content_filter")
                        )
                        && c[0].get("message").is_some_and(|m| {
                            m.get("content").is_some_and(Value::is_string)
                                && m.get("reasoning_content")
                                    .is_none_or(|v| v.is_null() || v.as_str() == Some(""))
                                && ["tool_calls", "function_call", "audio", "refusal"]
                                    .iter()
                                    .all(|k| m.get(k).is_none_or(Value::is_null))
                        })
                });
        let mut out = Self {
            completion_id,
            eligible,
            ..Self::default()
        };
        if !out.eligible {
            out.invalid.push("unsupported_completion_shape".into());
            return out;
        }
        let Some(u) = value.get("usage").and_then(Value::as_object) else {
            if value.get("usage").is_some_and(|u| !u.is_null()) {
                out.invalid.extend(["input".into(), "output".into()]);
            }
            return out;
        };
        out.input = quantity(u.get("prompt_tokens"), "input", &mut out.invalid);
        out.output = quantity(u.get("completion_tokens"), "output", &mut out.invalid);
        let total = quantity(u.get("total_tokens"), "total", &mut out.invalid);
        out.total = total;
        if out.invalid.iter().any(|s| s == "total")
            || total
                .zip(out.input.zip(out.output))
                .is_some_and(|(t, (i, o))| i.checked_add(o) != Some(t))
        {
            out.input = None;
            out.output = None;
            out.invalid.push("contradictory_total".into());
        }
        if out
            .input
            .zip(out.output)
            .is_some_and(|(i, o)| i.checked_add(o).is_none())
            || total.is_some_and(|t| {
                out.input.is_some_and(|i| i > t) || out.output.is_some_and(|o| o > t)
            })
        {
            out.input = None;
            out.output = None;
            out.invalid.push("contradictory_total".into());
        }
        let input = u.get("prompt_tokens_details");
        let output = u.get("completion_tokens_details");
        out.cached = quantity(
            input.and_then(|v| v.get("cached_tokens")),
            "cached",
            &mut out.invalid,
        );
        // DeepSeek exposes the cached subset at the usage root. Read the raw
        // field; never infer a cache hit count from an absent hit or miss field.
        let hit = quantity(u.get("prompt_cache_hit_tokens"), "cached", &mut out.invalid);
        match (out.cached, hit) {
            (Some(a), Some(b)) if a != b => {
                out.invalid.push("invalid_cache_breakdown".into());
            }
            (None, Some(b)) => out.cached = Some(b),
            _ => {}
        }
        let miss = quantity(
            u.get("prompt_cache_miss_tokens"),
            "invalid_cache_breakdown",
            &mut out.invalid,
        );
        out.cache_miss = miss;
        if miss.zip(out.input).is_some_and(|(m, i)| m > i)
            || miss
                .zip(out.cached)
                .zip(out.input)
                .is_some_and(|((m, c), i)| m.checked_add(c) != Some(i))
        {
            out.invalid.push("invalid_cache_breakdown".into());
        }
        out.reasoning = quantity(
            output.and_then(|v| v.get("reasoning_tokens")),
            "reasoning",
            &mut out.invalid,
        );
        for (detail, names, side) in [
            (
                input,
                &["audio_tokens", "image_tokens", "cache_write_tokens"][..],
                "input",
            ),
            (
                output,
                &[
                    "audio_tokens",
                    "accepted_prediction_tokens",
                    "rejected_prediction_tokens",
                ][..],
                "output",
            ),
        ] {
            if detail.is_some_and(|v| {
                !v.is_object()
                    || names
                        .iter()
                        .any(|n| v.get(n).is_some_and(|n| n.as_i64() != Some(0)))
            }) {
                out.invalid.push(format!("unsupported_{side}_details"));
                if side == "input" {
                    out.input = None;
                    out.cached = None;
                } else {
                    out.output = None;
                    out.reasoning = None;
                }
            }
        }
        if out.cached.zip(out.input).is_some_and(|(c, i)| c > i) {
            out.input = None;
            out.cached = None;
            out.invalid.push("cached_exceeds_input".into());
        }
        if out.reasoning.zip(out.output).is_some_and(|(r, o)| r > o) {
            out.output = None;
            out.reasoning = None;
            out.invalid.push("reasoning_exceeds_output".into());
        }
        out.reject_invalid_components();
        out
    }
}
/// Price only components whose quantities and selected rates are determined.
/// Returned usage describes billed quantities; `observed` remains the provider evidence.
pub fn price_observed_text(
    price: &Price,
    observed: &ObservedTextUsage,
    service_tier: Option<&str>,
    billable: bool,
) -> Result<(UsageRecord, ItemizedCharge, Vec<String>), WalletError> {
    let mut usage = UsageRecord {
        service_tier: service_tier.map(str::to_owned),
        ..UsageRecord::default()
    };
    let mut waived = Vec::new();
    if !billable || !observed.eligible {
        return Ok((
            usage.clone(),
            price.itemize(&usage, false)?,
            vec!["all_usage_and_request_fee".into()],
        ));
    }
    let input_rules_unknown = observed.input.is_none()
        && price.rules.iter().any(|r| {
            r.cond
                .service_tier
                .as_deref()
                .is_none_or(|t| Some(t) == service_tier)
                && (r.cond.min_input_tokens.is_some() || r.cond.max_input_tokens.is_some())
        });
    if input_rules_unknown {
        return Ok((
            usage.clone(),
            price.itemize(&usage, false)?,
            vec!["all_components_rate_selection_unknown".into()],
        ));
    }
    usage.input_tokens = observed.input.unwrap_or(0);
    let (set, matched_rule) = price.resolve(&usage);
    let input_known = observed.input.is_some()
        && (observed.cached.is_some()
            || set
                .cache_read_price_per_million
                .is_none_or(|r| r == set.input_price_per_million));
    let output_known = observed.output.is_some()
        && (observed.reasoning.is_some()
            || set
                .reasoning_price_per_million
                .is_none_or(|r| r == set.output_price_per_million));
    if input_known {
        usage.cached_tokens = observed.cached.unwrap_or(0);
    } else {
        usage.input_tokens = 0;
        waived.push("input_or_cached_split".into());
    }
    if output_known {
        usage.output_tokens = observed.output.unwrap_or(0);
        usage.reasoning_tokens = observed.reasoning.unwrap_or(0);
    } else {
        waived.push("output_or_reasoning_split".into());
    }
    usage.validate()?;
    let lines = set.lines(&usage, true);
    Ok((
        usage,
        ItemizedCharge {
            lines,
            matched_rule,
        },
        waived,
    ))
}

/// Supplier estimates require every relevant quantity and explicit historical supplier rates.
pub(super) fn supplier_cost(
    price: &Price,
    observed: &ObservedTextUsage,
    tier: Option<&str>,
) -> Result<Option<i64>, WalletError> {
    if !observed.eligible || !observed.invalid.is_empty() {
        return Ok(None);
    }
    let (Some(input), Some(output)) = (observed.input, observed.output) else {
        return Ok(None);
    };
    let mut usage = UsageRecord {
        input_tokens: input,
        output_tokens: output,
        service_tier: tier.map(str::to_owned),
        ..UsageRecord::default()
    };
    let (set, _) = price.resolve(&usage);
    let Some(rates) = set.upstream else {
        return Ok(None);
    };
    let (Some(input_rate), Some(output_rate)) = (
        rates.input_price_per_million,
        rates.output_price_per_million,
    ) else {
        return Ok(None);
    };
    if observed.cached.is_none()
        && rates
            .cache_read_price_per_million
            .is_some_and(|r| r != input_rate)
        || observed.reasoning.is_none()
            && rates
                .reasoning_price_per_million
                .is_some_and(|r| r != output_rate)
    {
        return Ok(None);
    }
    usage.cached_tokens = observed.cached.unwrap_or(0);
    usage.reasoning_tokens = observed.reasoning.unwrap_or(0);
    price.upstream_cost(&usage, true)
}
