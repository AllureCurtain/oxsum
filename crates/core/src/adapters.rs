//! Reading each upstream protocol's usage report into the normalized [`UsageRecord`].
//!
//! The gateway relays bytes; the adapter is the one place that knows what a usage
//! report looks like on a given protocol. Every adapter maps its wire shape into the
//! same record, so the pricing and ledger layers never see a protocol (D9,
//! docs/decisions.md): adding Anthropic means adding an implementation here, nothing
//! else.
//!
//! The invariant every adapter upholds is the record's own: `input_tokens` *includes*
//! the cached subset and `output_tokens` *includes* the reasoning subset. OpenAI,
//! DeepSeek and Gemini already report it that way; Anthropic does not — its
//! `cache_read_input_tokens` and `cache_creation_input_tokens` sit outside
//! `input_tokens` and its adapter adds them in (roadmap P5-1).

use serde_json::Value;

use crate::error::WalletError;
use crate::usage::UsageRecord;

/// The protocol a channel speaks when none is named, and the only one this build
/// knows: the OpenAI chat-completions wire format.
pub const OPENAI: &str = "openai";

/// What one protocol's usage report becomes: the normalized record.
pub trait UsageAdapter: Send + Sync {
    /// The name a channel's `protocol` column carries.
    fn protocol(&self) -> &'static str;

    /// The usage report in one upstream payload — a whole body, or one streamed
    /// chunk's JSON — normalized, or `None` when it carries no usable report.
    ///
    /// An adapter never fails: a payload that is not a report is simply no report,
    /// and a report with a negative count or a subset past its total is upstream
    /// being wrong, clamped or refused by the record's own rules.
    fn usage(&self, body: &Value) -> Option<UsageRecord>;
}

/// Resolves a channel's declared protocol to its adapter.
///
/// `None` names a protocol this build has no adapter for: a channel written that way
/// cannot serve, so the name is refused at write time rather than at the request.
#[must_use]
pub fn adapter_for(protocol: &str) -> Option<&'static dyn UsageAdapter> {
    match protocol {
        OPENAI => Some(&OpenAiChat),
        _ => None,
    }
}

/// The OpenAI chat-completions protocol — and the OpenAI-compatible providers that
/// speak it (DeepSeek, Groq and friends).
struct OpenAiChat;

impl UsageAdapter for OpenAiChat {
    fn protocol(&self) -> &'static str {
        OPENAI
    }

    /// `usage.prompt_tokens` already includes the cached subset and
    /// `usage.completion_tokens` the reasoning subset, so the details objects only
    /// fill columns — never add. The audio counts land in their own columns the
    /// same way: they sit inside the side's total, and the price book's
    /// fail-closed check flags them `unpriced` until a set prices them
    /// (issue #110). The usage object itself is kept verbatim as `provider_raw`
    /// for the reconciliation window; it never reaches a ledger description.
    fn usage(&self, body: &Value) -> Option<UsageRecord> {
        let usage = body.get("usage")?;
        let input = usage.get("prompt_tokens").and_then(Value::as_i64);
        let output = usage.get("completion_tokens").and_then(Value::as_i64);
        if input.is_none() && output.is_none() {
            return None;
        }
        let nested = |object: &str, field: &str| {
            usage
                .get(object)
                .and_then(|details| details.get(field))
                .and_then(Value::as_i64)
                .unwrap_or(0)
        };
        let record = UsageRecord {
            input_tokens: input.unwrap_or(0),
            output_tokens: output.unwrap_or(0),
            cached_tokens: nested("prompt_tokens_details", "cached_tokens"),
            audio_input_tokens: nested("prompt_tokens_details", "audio_tokens"),
            reasoning_tokens: nested("completion_tokens_details", "reasoning_tokens"),
            audio_output_tokens: nested("completion_tokens_details", "audio_tokens"),
            usage_details: Some(serde_json::json!({"provider_raw": usage.clone()})),
            ..UsageRecord::default()
        }
        .clamped();
        record.validate().ok()?;
        Some(record)
    }
}

/// The adapter a write must name: a protocol this build does not know is refused
/// before the row exists, because a channel that cannot normalize its own usage
/// reports cannot serve.
///
/// # Errors
///
/// [`WalletError::InvalidInput`] naming the unknown protocol.
pub fn known_protocol(protocol: &str) -> Result<(), WalletError> {
    if adapter_for(protocol).is_none() {
        return Err(WalletError::InvalidInput(format!(
            "unknown protocol: {protocol}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    // The tests may unwrap: a panic here is a failing test, which is what a test is for.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use serde_json::json;

    use super::*;

    fn openai() -> &'static dyn UsageAdapter {
        adapter_for(OPENAI).expect("the OpenAI adapter is registered")
    }

    #[test]
    fn the_registry_resolves_by_protocol_name() {
        assert_eq!(openai().protocol(), "openai");
        assert!(adapter_for("anthropic").is_none());
        assert!(known_protocol("openai").is_ok());
        assert!(known_protocol("anthropic").is_err());
    }

    #[test]
    fn only_a_payload_with_a_token_count_is_a_report() {
        assert!(
            openai()
                .usage(&json!({"choices": [], "usage": null}))
                .is_none()
        );
        assert!(openai().usage(&json!({"choices": []})).is_none());
        assert!(openai().usage(&json!({"usage": {}})).is_none());
        // A partial report is still a report: what upstream counted beats an estimate.
        let partial = openai()
            .usage(&json!({"usage": {"prompt_tokens": 3}}))
            .expect("partial usage");
        assert_eq!(partial.input_tokens, 3);
        assert_eq!(partial.output_tokens, 0);
    }

    #[test]
    fn the_openai_shape_fills_its_dimensions_and_keeps_the_raw_report() {
        let usage = openai()
            .usage(&json!({
                "usage": {
                    "prompt_tokens": 100,
                    "completion_tokens": 20,
                    "total_tokens": 120,
                    "prompt_tokens_details": {"cached_tokens": 80},
                    "completion_tokens_details": {"reasoning_tokens": 5},
                },
            }))
            .expect("a detailed report carries usage");
        // The subsets stay subsets: prompt_tokens already includes the cached part.
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.cached_tokens, 80);
        assert_eq!(usage.reasoning_tokens, 5);
        assert_eq!(
            usage.usage_details.as_ref().unwrap()["provider_raw"]["prompt_tokens"],
            100
        );
    }

    /// Audio counts land in their own columns — inside the side's total like the
    /// other details — so the price book's fail-closed check can flag them rather
    /// than have them vanish into `provider_raw` (issue #110).
    #[test]
    fn audio_counts_name_their_own_columns() {
        let usage = openai()
            .usage(&json!({
                "usage": {
                    "prompt_tokens": 100,
                    "completion_tokens": 20,
                    "prompt_tokens_details": {"audio_tokens": 60},
                    "completion_tokens_details": {"audio_tokens": 15},
                },
            }))
            .expect("an audio report carries usage");
        assert_eq!(usage.audio_input_tokens, 60);
        assert_eq!(usage.audio_output_tokens, 15);
        // A report without audio leaves the columns at zero.
        let quiet = openai()
            .usage(&json!({"usage": {"prompt_tokens": 100, "completion_tokens": 20}}))
            .expect("a plain report carries usage");
        assert_eq!(quiet.audio_input_tokens, 0);
        assert_eq!(quiet.audio_output_tokens, 0);
    }

    #[test]
    fn a_bad_report_is_clamped_or_refused_never_guessed() {
        // A subset reported past its total is clamped, not rejected: the counts are
        // still better than an estimate.
        let clamped = openai()
            .usage(&json!({
                "usage": {
                    "prompt_tokens": 10,
                    "prompt_tokens_details": {"cached_tokens": 40},
                },
            }))
            .expect("an over-subset report still carries usage");
        assert_eq!(clamped.cached_tokens, 10);
        // A negative count is no usable report at all.
        assert!(
            openai()
                .usage(&json!({"usage": {"prompt_tokens": -1}}))
                .is_none()
        );
    }
}
