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
//! `input_tokens` and its adapter adds them in.

use serde_json::Value;

use crate::error::WalletError;
use crate::usage::UsageRecord;

/// The protocol a channel speaks when none is named: the OpenAI chat-completions
/// wire format.
pub const OPENAI: &str = "openai";

/// The second protocol this build knows: Anthropic's Messages API.
pub const ANTHROPIC: &str = "anthropic";

/// What one protocol's usage report becomes: the normalized record.
pub trait UsageAdapter: Send + Sync {
    /// The name a channel's `protocol` column carries.
    fn protocol(&self) -> &'static str;

    /// The path under the channel's `base_url` the surface POSTs to —
    /// `chat/completions` for OpenAI, `messages` for Anthropic.
    fn upstream_path(&self) -> &'static str;

    /// The usage report in one upstream payload — a whole body, or one streamed
    /// chunk's JSON — normalized, or `None` when it carries no usable report.
    ///
    /// An adapter never fails: a payload that is not a report is simply no report,
    /// and a report with a negative count or a subset past its total is upstream
    /// being wrong, clamped or refused by the record's own rules.
    ///
    /// A protocol may report usage across several stream events — Anthropic's
    /// `message_start` knows the input side and `message_delta` the output side —
    /// so the relay folds successive reports into one record with
    /// [`UsageRecord::merge_report`].
    fn usage(&self, body: &Value) -> Option<UsageRecord>;

    /// The answer text one payload carries — a streamed chunk's delta or a whole
    /// body's message — the material a local estimate is priced from when no
    /// usage report arrives.
    fn answer_text(&self, body: &Value) -> String;

    /// Whether one `data:` payload is the stream's own terminator — OpenAI's
    /// `[DONE]` sentinel, Anthropic's `message_stop` (or `error`) event.
    fn ends_stream(&self, payload: &str) -> bool;

    /// The frame the relay emits when upstream ended without terminating its
    /// stream, so the client sees its protocol's own close either way.
    fn closing_frame(&self) -> &'static [u8];
}

/// Resolves a channel's declared protocol to its adapter.
///
/// `None` names a protocol this build has no adapter for: a channel written that way
/// cannot serve, so the name is refused at write time rather than at the request.
#[must_use]
pub fn adapter_for(protocol: &str) -> Option<&'static dyn UsageAdapter> {
    match protocol {
        OPENAI => Some(&OpenAiChat),
        ANTHROPIC => Some(&AnthropicMessages),
        _ => None,
    }
}

/// Resolves the upstream endpoint a request is relayed to — one protocol can
/// carry several: OpenAI-shaped channels answer chat completions, embeddings
/// and rerank, and each endpoint reports usage in its own shape (issue #170).
///
/// `endpoint` is the surface's own path spelling — `chat/completions`,
/// `messages`, `embeddings`, `rerank` — which the adapters also report through
/// [`UsageAdapter::upstream_path`]. `None` names a pairing this build cannot
/// meter: a surface never reaches one, because its own path is in the list.
#[must_use]
pub fn adapter_for_endpoint(protocol: &str, endpoint: &str) -> Option<&'static dyn UsageAdapter> {
    match (protocol, endpoint) {
        (OPENAI, "chat/completions") => Some(&OpenAiChat),
        (OPENAI, "embeddings") => Some(&OpenAiEmbeddings),
        (OPENAI, "rerank") => Some(&OpenAiRerank),
        (ANTHROPIC, "messages") => Some(&AnthropicMessages),
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

    fn upstream_path(&self) -> &'static str {
        "chat/completions"
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

    /// A streamed chunk carries its delta under `choices[].delta`, a whole
    /// body its message under `choices[].message`; the estimate needs whichever
    /// text the payload happens to carry.
    fn answer_text(&self, body: &Value) -> String {
        body.get("choices")
            .and_then(Value::as_array)
            .map(|choices| {
                choices
                    .iter()
                    .filter_map(|choice| {
                        choice
                            .get("delta")
                            .or_else(|| choice.get("message"))
                            .and_then(|message| message.get("content"))
                            .and_then(Value::as_str)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn ends_stream(&self, payload: &str) -> bool {
        payload == "[DONE]"
    }

    fn closing_frame(&self) -> &'static [u8] {
        b"data: [DONE]\n\n"
    }
}

/// The OpenAI embeddings endpoint (issue #170): the same protocol as chat —
/// Bearer credential, OpenAI-shaped errors — metered on input alone. Its usage
/// report is `usage.prompt_tokens`; `total_tokens` stands in when the prompt
/// count is absent, and the embeddings themselves are vectors, never answer
/// text, so a missing report settles estimated on the request's own bound.
struct OpenAiEmbeddings;

impl UsageAdapter for OpenAiEmbeddings {
    fn protocol(&self) -> &'static str {
        OPENAI
    }

    fn upstream_path(&self) -> &'static str {
        "embeddings"
    }

    fn usage(&self, body: &Value) -> Option<UsageRecord> {
        let usage = body.get("usage")?;
        let input = usage
            .get("prompt_tokens")
            .or_else(|| usage.get("total_tokens"))
            .and_then(Value::as_i64)?;
        let record = UsageRecord {
            input_tokens: input,
            usage_details: Some(serde_json::json!({"provider_raw": usage.clone()})),
            ..UsageRecord::default()
        }
        .clamped();
        record.validate().ok()?;
        Some(record)
    }

    /// An embeddings answer is vectors, not text: nothing to estimate from.
    fn answer_text(&self, _body: &Value) -> String {
        String::new()
    }

    /// The surface never streams, so no payload ends one; the impls exist for
    /// the trait's shape, not the wire's.
    fn ends_stream(&self, payload: &str) -> bool {
        payload == "[DONE]"
    }

    fn closing_frame(&self) -> &'static [u8] {
        b"data: [DONE]\n\n"
    }
}

/// The OpenAI-compatible rerank endpoint (issue #170): Jina's and Cohere's
/// shared shape — `query` plus `documents`, ranked back with a
/// `usage.total_tokens` report that counts query and documents together as the
/// billed input.
struct OpenAiRerank;

impl UsageAdapter for OpenAiRerank {
    fn protocol(&self) -> &'static str {
        OPENAI
    }

    fn upstream_path(&self) -> &'static str {
        "rerank"
    }

    fn usage(&self, body: &Value) -> Option<UsageRecord> {
        let usage = body.get("usage")?;
        let input = usage.get("total_tokens").and_then(Value::as_i64)?;
        let record = UsageRecord {
            input_tokens: input,
            usage_details: Some(serde_json::json!({"provider_raw": usage.clone()})),
            ..UsageRecord::default()
        }
        .clamped();
        record.validate().ok()?;
        Some(record)
    }

    /// A rerank answer is scores, not text: nothing to estimate from.
    fn answer_text(&self, _body: &Value) -> String {
        String::new()
    }

    fn ends_stream(&self, payload: &str) -> bool {
        payload == "[DONE]"
    }

    fn closing_frame(&self) -> &'static [u8] {
        b"data: [DONE]\n\n"
    }
}

/// The Anthropic Messages protocol.
///
/// Where OpenAI reports usage once at the end of a stream, Anthropic reports
/// it twice: `message_start` knows the input side (including both cache counts)
/// and `message_delta` carries cumulative `output_tokens`. The relay merges the
/// two into one record, so each report only needs to say what it knows.
struct AnthropicMessages;

impl UsageAdapter for AnthropicMessages {
    fn protocol(&self) -> &'static str {
        ANTHROPIC
    }

    fn upstream_path(&self) -> &'static str {
        "messages"
    }

    /// Anthropic's `usage.input_tokens` *excludes* the cache counts — the
    /// opposite of OpenAI — so the normalized record adds `cache_read` and
    /// `cache_creation` back in for the record's own invariant to hold. The
    /// creation lump splits across the two write tiers when `cache_creation`
    /// details it; a lump with no split lands in the 5-minute tier.
    fn usage(&self, body: &Value) -> Option<UsageRecord> {
        let usage = if body.get("type").and_then(Value::as_str) == Some("message_start") {
            body.get("message")?.get("usage")?
        } else {
            body.get("usage")?
        };
        let count = |field: &str| usage.get(field).and_then(Value::as_i64);
        let input = count("input_tokens");
        let output = count("output_tokens");
        if input.is_none() && output.is_none() {
            return None;
        }
        let cache_read = count("cache_read_input_tokens").unwrap_or(0);
        let cache_write = count("cache_creation_input_tokens").unwrap_or(0);
        let split = |field: &str| {
            usage
                .get("cache_creation")
                .and_then(|detail| detail.get(field))
                .and_then(Value::as_i64)
                .unwrap_or(0)
        };
        let write_1h = split("ephemeral_1h_input_tokens");
        let write_5m = {
            let detailed = split("ephemeral_5m_input_tokens");
            if detailed > 0 {
                detailed
            } else {
                (cache_write - write_1h).max(0)
            }
        };
        let record = UsageRecord {
            input_tokens: input.unwrap_or(0) + cache_read + cache_write,
            output_tokens: output.unwrap_or(0),
            cached_tokens: cache_read,
            cache_write_5m_tokens: write_5m,
            cache_write_1h_tokens: write_1h,
            usage_details: Some(serde_json::json!({"provider_raw": usage.clone()})),
            ..UsageRecord::default()
        }
        .clamped();
        record.validate().ok()?;
        Some(record)
    }

    /// A `content_block_delta` frame carries its text — or a tool call's
    /// `partial_json` — under `delta`; a whole body under `content[]`'s text
    /// blocks.
    fn answer_text(&self, body: &Value) -> String {
        if body.get("type").and_then(Value::as_str) == Some("content_block_delta") {
            return body
                .get("delta")
                .and_then(|delta| delta.get("text").or_else(|| delta.get("partial_json")))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
        }
        body.get("content")
            .and_then(Value::as_array)
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|block| block.get("text").and_then(Value::as_str))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// `message_stop` ends a normal stream and `error` an abnormal one; either
    /// way upstream has nothing more to say.
    fn ends_stream(&self, payload: &str) -> bool {
        let Ok(value) = serde_json::from_str::<Value>(payload) else {
            return false;
        };
        matches!(
            value.get("type").and_then(Value::as_str),
            Some("message_stop" | "error")
        )
    }

    fn closing_frame(&self) -> &'static [u8] {
        b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
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

    fn anthropic() -> &'static dyn UsageAdapter {
        adapter_for(ANTHROPIC).expect("the Anthropic adapter is registered")
    }

    #[test]
    fn the_registry_resolves_by_protocol_name() {
        assert_eq!(openai().protocol(), "openai");
        assert_eq!(anthropic().protocol(), "anthropic");
        assert!(adapter_for("gemini").is_none());
        assert!(known_protocol("openai").is_ok());
        assert!(known_protocol("anthropic").is_ok());
        assert!(known_protocol("gemini").is_err());
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

    /// Anthropic's `input_tokens` excludes both cache counts, so the normalized
    /// record folds them back in — the record's own invariant that every input
    /// subset sits inside `input_tokens`.
    #[test]
    fn the_anthropic_shape_folds_cache_counts_into_the_input_total() {
        let usage = anthropic()
            .usage(&json!({
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 20,
                    "cache_read_input_tokens": 60,
                    "cache_creation_input_tokens": 40,
                    "cache_creation": {
                        "ephemeral_5m_input_tokens": 30,
                        "ephemeral_1h_input_tokens": 10,
                    },
                },
            }))
            .expect("an anthropic report carries usage");
        assert_eq!(usage.input_tokens, 200);
        assert_eq!(usage.output_tokens, 20);
        assert_eq!(usage.cached_tokens, 60);
        assert_eq!(usage.cache_write_5m_tokens, 30);
        assert_eq!(usage.cache_write_1h_tokens, 10);
        assert_eq!(
            usage.usage_details.as_ref().unwrap()["provider_raw"]["input_tokens"],
            100
        );
    }

    /// A `cache_creation_input_tokens` lump with no tier split counts toward the
    /// 5-minute tier — the common case, and never billed at zero.
    #[test]
    fn an_unsplit_cache_write_lump_lands_in_the_five_minute_tier() {
        let usage = anthropic()
            .usage(&json!({
                "usage": {
                    "input_tokens": 100,
                    "cache_creation_input_tokens": 40,
                },
            }))
            .expect("a lump cache write still carries usage");
        assert_eq!(usage.input_tokens, 140);
        assert_eq!(usage.cache_write_5m_tokens, 40);
        assert_eq!(usage.cache_write_1h_tokens, 0);
    }

    /// The two stream events each carry a partial report: `message_start` knows
    /// the input side, `message_delta` the cumulative output side. Neither is a
    /// complete record alone — the relay merges them.
    #[test]
    fn the_anthropic_stream_events_report_their_halves() {
        let start = anthropic()
            .usage(&json!({
                "type": "message_start",
                "message": {"usage": {"input_tokens": 50, "output_tokens": 1}},
            }))
            .expect("message_start carries the input side");
        assert_eq!(start.input_tokens, 50);
        assert_eq!(start.output_tokens, 1);
        let delta = anthropic()
            .usage(&json!({
                "type": "message_delta",
                "usage": {"output_tokens": 42},
            }))
            .expect("message_delta carries the output side");
        assert_eq!(delta.output_tokens, 42);
        // A frame with no usage object at all is no report.
        assert!(
            anthropic()
                .usage(&json!({"type": "content_block_delta", "delta": {"text": "hi"}}))
                .is_none()
        );
        assert!(
            anthropic()
                .usage(&json!({"type": "message_stop"}))
                .is_none()
        );
    }

    #[test]
    fn anthropic_text_comes_from_deltas_and_text_blocks() {
        assert_eq!(
            anthropic().answer_text(&json!({
                "type": "content_block_delta",
                "delta": {"type": "text_delta", "text": "hel"},
            })),
            "hel"
        );
        assert_eq!(
            anthropic().answer_text(&json!({
                "type": "content_block_delta",
                "delta": {"type": "input_json_delta", "partial_json": "{\"x\":"},
            })),
            "{\"x\":"
        );
        assert_eq!(
            anthropic().answer_text(&json!({
                "content": [
                    {"type": "text", "text": "one"},
                    {"type": "tool_use", "name": "t"},
                    {"type": "text", "text": "two"},
                ],
            })),
            "onetwo"
        );
        assert_eq!(anthropic().answer_text(&json!({"type": "ping"})), "");
    }

    #[test]
    fn each_protocol_knows_its_own_stream_close() {
        assert!(openai().ends_stream("[DONE]"));
        assert!(!openai().ends_stream("{\"choices\":[]}"));
        assert_eq!(openai().closing_frame(), b"data: [DONE]\n\n");
        assert!(anthropic().ends_stream("{\"type\":\"message_stop\"}"));
        assert!(anthropic().ends_stream("{\"type\":\"error\"}"));
        assert!(!anthropic().ends_stream("{\"type\":\"ping\"}"));
        assert!(!anthropic().ends_stream("[DONE]"));
        assert_eq!(
            anthropic().closing_frame(),
            b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
        );
    }

    // ── the input-only endpoints (issue #170) ────────────────────────────────

    #[test]
    fn the_endpoint_registry_pairs_protocols_and_paths() {
        assert_eq!(
            adapter_for_endpoint(OPENAI, "chat/completions")
                .unwrap()
                .upstream_path(),
            "chat/completions"
        );
        assert_eq!(
            adapter_for_endpoint(OPENAI, "embeddings")
                .unwrap()
                .upstream_path(),
            "embeddings"
        );
        assert_eq!(
            adapter_for_endpoint(OPENAI, "rerank")
                .unwrap()
                .upstream_path(),
            "rerank"
        );
        assert_eq!(
            adapter_for_endpoint(ANTHROPIC, "messages")
                .unwrap()
                .upstream_path(),
            "messages"
        );
        // Crossed pairings name nothing this build meters.
        assert!(adapter_for_endpoint(ANTHROPIC, "embeddings").is_none());
        assert!(adapter_for_endpoint(OPENAI, "messages").is_none());
        assert!(adapter_for_endpoint("gemini", "embeddings").is_none());
    }

    #[test]
    fn embeddings_usage_reads_the_prompt_count() {
        let adapter = adapter_for_endpoint(OPENAI, "embeddings").unwrap();
        let usage = adapter
            .usage(&json!({
                "object": "list",
                "data": [{"object": "embedding", "index": 0, "embedding": [0.1]}],
                "model": "text-embedding-3-small",
                "usage": {"prompt_tokens": 25, "total_tokens": 25},
            }))
            .expect("an embeddings answer reports usage");
        assert_eq!(usage.input_tokens, 25);
        assert_eq!(usage.output_tokens, 0);
        // The provider's raw report rides usage_details for the anomalies view.
        assert_eq!(
            usage.usage_details.unwrap()["provider_raw"]["total_tokens"],
            25
        );

        // `total_tokens` stands in when `prompt_tokens` is absent — some
        // providers report only the total.
        assert_eq!(
            adapter
                .usage(&json!({"usage": {"total_tokens": 9}}))
                .unwrap()
                .input_tokens,
            9
        );
        // No usage object is no report: the turn settles estimated.
        assert!(adapter.usage(&json!({"data": []})).is_none());
        // Vectors are not answer text — nothing to estimate an answer from.
        assert_eq!(adapter.answer_text(&json!({"data": []})), "");
    }

    #[test]
    fn rerank_usage_reads_the_total_count() {
        let adapter = adapter_for_endpoint(OPENAI, "rerank").unwrap();
        let usage = adapter
            .usage(&json!({
                "model": "rerank-v3.5",
                "results": [{"index": 0, "relevance_score": 0.9}],
                "usage": {"total_tokens": 42},
            }))
            .expect("a rerank answer reports usage");
        assert_eq!(usage.input_tokens, 42);
        assert_eq!(usage.output_tokens, 0);
        assert!(adapter.usage(&json!({"results": []})).is_none());
        assert_eq!(adapter.answer_text(&json!({"results": []})), "");
    }
}
