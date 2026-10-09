//! A gateway request, as far as the gateway reads it.
//!
//! Two surfaces parse into the same shape: `/v1/chat/completions` speaks OpenAI's wire
//! format, `/v1/messages` Anthropic's. Everything the gateway does not act on is forwarded
//! upstream unchanged, which is what keeps an SDK working here by changing `base_url` only.
//! The fields it does act on are taken out and validated once, so the rest of the request
//! path never has to pick apart JSON.

use oxsum_core::{
    ANTHROPIC, Attribution, BillingMode, MAX_CONTEXT_NAME, MAX_END_USER, MAX_TAG, MAX_TAGS, OPENAI,
};
use serde_json::{Map, Value, json};

use super::error::GatewayError;

/// Which client protocol a request arrived under. Both surfaces run the same
/// freeze-then-settle pipeline; the surface chooses the request's dialect, which
/// channels may serve it, and which error envelope the client reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// `POST /v1/chat/completions` — OpenAI's shapes, served by `openai` channels.
    OpenAi,
    /// `POST /v1/messages` — Anthropic's shapes, served by `anthropic` channels.
    Anthropic,
    /// `POST /v1/embeddings` — OpenAI's input-only embeddings shape (issue #170).
    Embeddings,
    /// `POST /v1/rerank` — the Jina/Cohere-shaped rerank call (issue #170).
    Rerank,
}

impl Surface {
    /// The protocol name a serving channel must declare for this surface. A model
    /// an `anthropic` channel serves is not served on the OpenAI surface, and vice
    /// versa: surfaces are protocol-native, never translated. The input-only
    /// surfaces speak the OpenAI protocol — Bearer credential, OpenAI errors.
    pub fn protocol(self) -> &'static str {
        match self {
            Self::OpenAi | Self::Embeddings | Self::Rerank => OPENAI,
            Self::Anthropic => ANTHROPIC,
        }
    }

    /// The billing mode a serving price must carry: a model priced only for
    /// chat is not served on the input-only surfaces, and vice versa — a mode
    /// mismatch fails closed, never reinterprets a rate (issue #170).
    pub fn billing_mode(self) -> BillingMode {
        match self {
            Self::OpenAi | Self::Anthropic => BillingMode::Chat,
            Self::Embeddings => BillingMode::Embeddings,
            Self::Rerank => BillingMode::Rerank,
        }
    }

    /// The path under the channel's `base_url` the surface POSTs to — also the
    /// key [`adapter_for_endpoint`](oxsum_core::adapter_for_endpoint) resolves
    /// the usage dialect by.
    pub fn upstream_path(self) -> &'static str {
        match self {
            Self::OpenAi => "chat/completions",
            Self::Anthropic => "messages",
            Self::Embeddings => "embeddings",
            Self::Rerank => "rerank",
        }
    }
}

/// One parsed gateway request: the body to forward, and the parts the freeze depends on.
#[derive(Debug, Clone)]
pub struct GatewayRequest {
    /// The caller's body, minus nothing: what is sent upstream is this, with the output ceiling
    /// written into it.
    body: Map<String, Value>,
    /// The surface the request arrived under.
    surface: Surface,
    /// The model the caller asked for, which must have a configured price.
    pub model: String,
    /// Whether the caller asked for a streamed answer.
    pub stream: bool,
    /// The text of every message, for the input upper bound and the local estimate. A message with
    /// no text (an assistant turn that only calls a tool) contributes none.
    pub texts: Vec<String>,
    /// The output ceiling the caller asked for, if it asked for one. The
    /// input-only surfaces have no output side, so this stays `None` there.
    pub max_tokens: Option<i64>,
    /// The request's exact input token count when the caller sent token arrays
    /// instead of text — an embeddings `input` of `[usize]` ids freezes at its
    /// length, not at a text estimate (issue #170).
    pub counted_input: Option<i64>,
    /// The caller's attribution on the turn: OpenAI's `user`/`metadata`/`service_tier`,
    /// Anthropic's `metadata.user_id` — recorded on the usage row, never an input
    /// to the freeze or the price.
    pub attribution: Attribution,
}

impl GatewayRequest {
    /// Reads a request body in the surface's own dialect.
    ///
    /// # Errors
    ///
    /// Refuses a body that is not an object, a missing or empty `model`, a missing or empty
    /// `messages`/`input`/`documents`, a non-boolean `stream` — true on the input-only
    /// surfaces, which never stream — a non-integer output ceiling, and any content without
    /// a computable input bound: the freeze is only a promise if the input bound is
    /// computable (product.md).
    pub fn parse(body: Value, surface: Surface) -> Result<Self, GatewayError> {
        let Value::Object(body) = body else {
            return Err(invalid("the request body must be a JSON object"));
        };
        let model = match body.get("model") {
            Some(Value::String(model)) if !model.trim().is_empty() => model.clone(),
            _ => {
                return Err(param(
                    "model",
                    "model is required and must be a non-empty string",
                ));
            }
        };
        let stream = match body.get("stream") {
            None | Some(Value::Null) | Some(Value::Bool(false)) => false,
            Some(Value::Bool(true)) => {
                if surface.billing_mode().meters_output() {
                    true
                } else {
                    return Err(param("stream", "this surface does not stream"));
                }
            }
            Some(_) => return Err(param("stream", "stream must be a boolean")),
        };
        let (texts, counted_input, max_tokens, attribution) = match surface {
            Surface::OpenAi => (
                texts_of(&body)?,
                None,
                output_ceiling(&body)?,
                attribution_of(&body)?,
            ),
            Surface::Anthropic => (
                anthropic_texts(&body)?,
                None,
                int_ceiling(&body, "max_tokens")?,
                anthropic_attribution(&body)?,
            ),
            Surface::Embeddings => {
                let (texts, counted) = embeddings_input(&body)?;
                (texts, counted, None, attribution_of(&body)?)
            }
            Surface::Rerank => (rerank_texts(&body)?, None, None, attribution_of(&body)?),
        };
        Ok(Self {
            body,
            surface,
            model,
            stream,
            texts,
            max_tokens,
            counted_input,
            attribution,
        })
    }

    /// The body to send upstream: the caller's, with the output upper bound written in and usage
    /// reporting switched on for a stream.
    ///
    /// The bound is written as `max_tokens` — the field both protocols name — and any
    /// `max_completion_tokens` is dropped, so upstream sees exactly one ceiling, the one the
    /// freeze was computed for. An OpenAI stream asks for usage explicitly, because the last
    /// chunk is where that protocol reports it; Anthropic reports usage on every stream
    /// already, so nothing is added there. The input-only surfaces have no output
    /// bound to write: their bodies forward untouched (issue #170).
    #[must_use]
    pub fn forwarded(&self, output_bound: i64) -> Value {
        let mut body = self.body.clone();
        if self.surface.billing_mode().meters_output() {
            body.insert("max_tokens".to_owned(), json!(output_bound));
            body.remove("max_completion_tokens");
            if self.stream && self.surface == Surface::OpenAi {
                let mut options = body
                    .get("stream_options")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                options.insert("include_usage".to_owned(), Value::Bool(true));
                body.insert("stream_options".to_owned(), Value::Object(options));
            }
        }
        Value::Object(body)
    }
}

/// The embeddings `input`: a string, an array of strings, an array of token
/// ids, or an array of token-id arrays. Token arrays give an exact count for
/// the freeze; strings give texts for the estimate bound (issue #170).
fn embeddings_input(body: &Map<String, Value>) -> Result<(Vec<String>, Option<i64>), GatewayError> {
    let Some(input) = body.get("input") else {
        return Err(param("input", "input is required"));
    };
    match input {
        Value::String(text) => Ok((vec![text.clone()], None)),
        Value::Array(items) => {
            if items.is_empty() {
                return Err(param("input", "input must not be empty"));
            }
            match items.as_slice() {
                // The all-integers spellings are token arrays: their count is
                // exact, so the freeze uses it rather than a text estimate.
                [Value::Array(_), ..] => {
                    let mut total = 0_i64;
                    for item in items {
                        let Value::Array(tokens) = item else {
                            return Err(param("input", "token inputs must be arrays of integers"));
                        };
                        if tokens.is_empty() {
                            return Err(param("input", "a token array must not be empty"));
                        }
                        for token in tokens {
                            if token.as_u64().is_none() {
                                return Err(param(
                                    "input",
                                    "a token array carries non-negative integers",
                                ));
                            }
                        }
                        total = total
                            .checked_add(
                                i64::try_from(tokens.len())
                                    .map_err(|_| invalid("input is too large"))?,
                            )
                            .ok_or_else(|| invalid("input is too large"))?;
                    }
                    Ok((Vec::new(), Some(total)))
                }
                [Value::Number(_), ..] => {
                    for token in items {
                        if token.as_u64().is_none() {
                            return Err(param(
                                "input",
                                "a token array carries non-negative integers",
                            ));
                        }
                    }
                    let total =
                        i64::try_from(items.len()).map_err(|_| invalid("input is too large"))?;
                    Ok((Vec::new(), Some(total)))
                }
                _ => {
                    let mut texts = Vec::with_capacity(items.len());
                    for item in items {
                        let Value::String(text) = item else {
                            return Err(param(
                                "input",
                                "input must be a string, an array of strings, or token arrays",
                            ));
                        };
                        texts.push(text.clone());
                    }
                    Ok((texts, None))
                }
            }
        }
        _ => Err(param(
            "input",
            "input must be a string, an array of strings, or token arrays",
        )),
    }
}

/// The rerank request's billable text: `query` plus every document's text —
/// a string entry's own value, an object entry's `text`. A document of any
/// other shape has no computable bound and is refused (issue #170).
fn rerank_texts(body: &Map<String, Value>) -> Result<Vec<String>, GatewayError> {
    let mut texts = Vec::new();
    match body.get("query") {
        Some(Value::String(query)) => texts.push(query.clone()),
        Some(_) => return Err(param("query", "query must be a string")),
        None => return Err(param("query", "query is required")),
    }
    match body.get("documents") {
        Some(Value::Array(documents)) if !documents.is_empty() => {
            for document in documents {
                match document {
                    Value::String(text) => texts.push(text.clone()),
                    Value::Object(document) => match document.get("text") {
                        Some(Value::String(text)) => texts.push(text.clone()),
                        _ => {
                            return Err(param(
                                "documents",
                                "a document object must carry a text string",
                            ));
                        }
                    },
                    _ => {
                        return Err(param(
                            "documents",
                            "a document must be a string or an object carrying text",
                        ));
                    }
                }
            }
        }
        Some(Value::Array(_)) => {
            return Err(param("documents", "documents must not be empty"));
        }
        Some(_) => return Err(param("documents", "documents must be an array")),
        None => return Err(param("documents", "documents is required")),
    }
    Ok(texts)
}

/// The text of every message, refusing content that is not text.
fn texts_of(body: &Map<String, Value>) -> Result<Vec<String>, GatewayError> {
    let Some(messages) = body.get("messages") else {
        return Err(param("messages", "messages is required"));
    };
    let Value::Array(messages) = messages else {
        return Err(param("messages", "messages must be an array"));
    };
    if messages.is_empty() {
        return Err(param("messages", "messages must not be empty"));
    }
    let mut texts = Vec::new();
    for message in messages {
        let Value::Object(message) = message else {
            return Err(param("messages", "every message must be an object"));
        };
        match message.get("content") {
            // An assistant turn that only asks for a tool call has no text.
            None | Some(Value::Null) => {}
            Some(Value::String(text)) => texts.push(text.clone()),
            Some(Value::Array(parts)) => {
                for part in parts {
                    let Value::Object(part) = part else {
                        return Err(param("messages", "a content part must be an object"));
                    };
                    match part.get("type").and_then(Value::as_str) {
                        Some("text") => match part.get("text") {
                            Some(Value::String(text)) => texts.push(text.clone()),
                            _ => {
                                return Err(param(
                                    "messages",
                                    "a text content part must carry a text string",
                                ));
                            }
                        },
                        _ => {
                            return Err(param(
                                "messages",
                                "only text content is supported: an image or another content part \
                                 has no computable input bound, so it cannot be frozen",
                            ));
                        }
                    }
                }
            }
            Some(_) => {
                return Err(param(
                    "messages",
                    "message content must be a string or an array of content parts",
                ));
            }
        }
    }
    Ok(texts)
}

/// The text of an Anthropic request: `system` plus every message's content.
///
/// The content-block rule is the OpenAI surface's rule widened one notch: a `text`
/// block contributes its `text`; a block with any other non-media `type` — a tool
/// call, a tool result, a thinking block — contributes its serialized JSON, because
/// that is a computable bound for whatever it carries; a media block (`image`,
/// `document`, `audio`, `video`) has no computable bound and is refused.
fn anthropic_texts(body: &Map<String, Value>) -> Result<Vec<String>, GatewayError> {
    let mut texts = Vec::new();
    match body.get("system") {
        None | Some(Value::Null) => {}
        Some(Value::String(text)) => texts.push(text.clone()),
        Some(Value::Array(blocks)) => anthropic_blocks(blocks, "system", &mut texts)?,
        Some(_) => {
            return Err(param(
                "system",
                "system must be a string or an array of content blocks",
            ));
        }
    }
    let Some(messages) = body.get("messages") else {
        return Err(param("messages", "messages is required"));
    };
    let Value::Array(messages) = messages else {
        return Err(param("messages", "messages must be an array"));
    };
    if messages.is_empty() {
        return Err(param("messages", "messages must not be empty"));
    }
    for message in messages {
        let Value::Object(message) = message else {
            return Err(param("messages", "every message must be an object"));
        };
        match message.get("content") {
            None | Some(Value::Null) => {}
            Some(Value::String(text)) => texts.push(text.clone()),
            Some(Value::Array(blocks)) => anthropic_blocks(blocks, "messages", &mut texts)?,
            Some(_) => {
                return Err(param(
                    "messages",
                    "message content must be a string or an array of content blocks",
                ));
            }
        }
    }
    Ok(texts)
}

/// Folds one array of Anthropic content blocks into the request's texts.
fn anthropic_blocks(
    blocks: &[Value],
    field: &'static str,
    texts: &mut Vec<String>,
) -> Result<(), GatewayError> {
    for block in blocks {
        let Value::Object(block) = block else {
            return Err(param(field, "a content block must be an object"));
        };
        match block.get("type").and_then(Value::as_str) {
            Some("text") => match block.get("text") {
                Some(Value::String(text)) => texts.push(text.clone()),
                _ => {
                    return Err(param(field, "a text block must carry a text string"));
                }
            },
            Some("image" | "document" | "audio" | "video") => {
                return Err(param(
                    field,
                    "only text-computable content is supported: a media block has no \
                     computable input bound, so it cannot be frozen",
                ));
            }
            // A `tool_use` input, a `tool_result`, a thinking block: serialized JSON is
            // the computable bound for whatever the block carries.
            _ => texts.push(Value::Object(block.clone()).to_string()),
        }
    }
    Ok(())
}

/// Anthropic's attribution: `metadata.user_id` is the only field the surface reads,
/// landing on the usage record's `end_user`; the rest of `metadata` is upstream's.
fn anthropic_attribution(body: &Map<String, Value>) -> Result<Attribution, GatewayError> {
    let end_user = match body.get("metadata") {
        None | Some(Value::Null) => None,
        Some(Value::Object(metadata)) => match metadata.get("user_id") {
            None | Some(Value::Null) => None,
            Some(Value::String(user)) if user.len() <= MAX_END_USER => Some(user.clone()),
            Some(Value::String(_)) => {
                return Err(param(
                    "metadata",
                    "metadata.user_id is at most 128 characters",
                ));
            }
            Some(_) => return Err(param("metadata", "metadata.user_id must be a string")),
        },
        Some(_) => return Err(param("metadata", "metadata must be an object")),
    };
    Ok(Attribution {
        end_user,
        ..Attribution::default()
    })
}

/// One integer output ceiling under a single name — Anthropic's `max_tokens`.
/// `null` counts as absent, because SDKs send unset optional fields that way.
fn int_ceiling(body: &Map<String, Value>, name: &'static str) -> Result<Option<i64>, GatewayError> {
    match body.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => match value.as_i64() {
            Some(asked) => Ok(Some(asked)),
            None => Err(param(
                name,
                "max_tokens must be an integer number of tokens",
            )),
        },
    }
}

/// The caller's output ceiling: `max_completion_tokens` if it used the newer spelling, else
/// `max_tokens`; `null` counts as absent, because SDKs send unset optional fields that way.
fn output_ceiling(body: &Map<String, Value>) -> Result<Option<i64>, GatewayError> {
    for name in ["max_completion_tokens", "max_tokens"] {
        match body.get(name) {
            None | Some(Value::Null) => {}
            Some(value) => match value.as_i64() {
                Some(asked) => return Ok(Some(asked)),
                None => {
                    return Err(param(
                        "max_tokens",
                        "max_tokens must be an integer number of tokens",
                    ));
                }
            },
        }
    }
    Ok(None)
}

/// The caller's attribution: `user`, `metadata` and `service_tier`, bounded the way the
/// usage record bounds them. Writes are free within the bounds — these name the
/// customer's own users, so there is no allowlist — and everything past a bound is a
/// 400, because silently truncating an id would bill it under the wrong name.
fn attribution_of(body: &Map<String, Value>) -> Result<Attribution, GatewayError> {
    let end_user = match body.get("user") {
        None | Some(Value::Null) => None,
        Some(Value::String(user)) if user.len() <= MAX_END_USER => Some(user.clone()),
        Some(Value::String(_)) => {
            return Err(param("user", "user is at most 128 characters"));
        }
        Some(_) => return Err(param("user", "user must be a string")),
    };
    let mut tags = std::collections::BTreeMap::new();
    match body.get("metadata") {
        None | Some(Value::Null) => {}
        Some(Value::Object(metadata)) => {
            if metadata.len() > MAX_TAGS {
                return Err(param("metadata", "metadata carries at most 10 entries"));
            }
            for (key, value) in metadata {
                if key.is_empty() || key.len() > MAX_TAG {
                    return Err(param("metadata", "a metadata key is 1-64 characters"));
                }
                match value {
                    Value::String(value) if value.len() <= MAX_TAG => {
                        tags.insert(key.clone(), value.clone());
                    }
                    Value::String(_) => {
                        return Err(param(
                            "metadata",
                            "a metadata value is at most 64 characters",
                        ));
                    }
                    _ => return Err(param("metadata", "metadata values must be strings")),
                }
            }
        }
        Some(_) => return Err(param("metadata", "metadata must be an object")),
    }
    let service_tier = match body.get("service_tier") {
        None | Some(Value::Null) => None,
        Some(Value::String(tier)) if tier.len() <= MAX_CONTEXT_NAME => Some(tier.clone()),
        Some(Value::String(_)) => {
            return Err(param(
                "service_tier",
                "service_tier is at most 64 characters",
            ));
        }
        Some(_) => return Err(param("service_tier", "service_tier must be a string")),
    };
    Ok(Attribution {
        end_user,
        tags,
        service_tier,
    })
}

fn invalid(message: impl Into<String>) -> GatewayError {
    GatewayError::Invalid {
        message: message.into(),
        param: None,
    }
}

fn param(which: &'static str, message: impl Into<String>) -> GatewayError {
    GatewayError::Invalid {
        message: message.into(),
        param: Some(which),
    }
}

#[cfg(test)]
mod tests {
    // The tests may unwrap: a panic here is a failing test, which is what a test is for.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn request(body: Value) -> Result<GatewayRequest, GatewayError> {
        GatewayRequest::parse(body, Surface::OpenAi)
    }

    fn messages_request(body: Value) -> Result<GatewayRequest, GatewayError> {
        GatewayRequest::parse(body, Surface::Anthropic)
    }

    #[test]
    fn reads_a_plain_request() {
        let parsed = request(json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
        }))
        .unwrap();
        assert_eq!(parsed.model, "m");
        assert!(!parsed.stream);
        assert_eq!(parsed.texts, ["hi"]);
        assert_eq!(parsed.max_tokens, None);
    }

    #[test]
    fn keeps_unknown_fields_for_upstream() {
        let parsed = request(json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "temperature": 0.2,
            "tools": [{"type": "function"}],
        }))
        .unwrap();
        let forwarded = parsed.forwarded(128);
        assert_eq!(forwarded["temperature"], 0.2);
        assert_eq!(forwarded["tools"][0]["type"], "function");
        assert_eq!(forwarded["max_tokens"], 128);
    }

    #[test]
    fn refuses_what_it_cannot_price() {
        assert!(request(json!("nonsense")).is_err());
        assert!(request(json!({"messages": []})).is_err());
        assert!(request(json!({"model": " ", "messages": []})).is_err());
        assert!(request(json!({"model": "m"})).is_err());
        assert!(request(json!({"model": "m", "messages": []})).is_err());
        assert!(request(json!({"model": "m", "messages": [1]})).is_err());
        assert!(
            request(json!({
                "model": "m",
                "messages": [{"role": "user", "content": 7}],
            }))
            .is_err()
        );
        assert!(
            request(json!({
                "model": "m",
                "messages": [{"role": "user", "content": [{"type": "text"}]}],
            }))
            .is_err()
        );
    }

    #[test]
    fn refuses_an_image_because_the_input_bound_would_be_a_guess() {
        let error = request(json!({
            "model": "m",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "what is this"},
                    {"type": "image_url", "image_url": {"url": "https://example.test/x.png"}},
                ],
            }],
        }))
        .expect_err("an image has no computable input bound");
        let GatewayError::Invalid { message, param } = error else {
            panic!("expected an invalid-request error");
        };
        assert_eq!(param, Some("messages"));
        assert!(message.contains("only text content"), "{message}");
    }

    #[test]
    fn reads_content_parts_and_an_absent_content() {
        let parsed = request(json!({
            "model": "m",
            "messages": [
                {"role": "system", "content": [{"type": "text", "text": "be brief"}]},
                {"role": "assistant", "content": null, "tool_calls": []},
                {"role": "user", "content": "hi"},
            ],
        }))
        .unwrap();
        assert_eq!(parsed.texts, ["be brief", "hi"]);
    }

    #[test]
    fn the_newer_output_ceiling_wins() {
        let parsed = request(json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 10,
            "max_completion_tokens": 20,
        }))
        .unwrap();
        assert_eq!(parsed.max_tokens, Some(20));
        let forwarded = parsed.forwarded(20);
        assert_eq!(forwarded["max_tokens"], 20);
        assert!(forwarded.get("max_completion_tokens").is_none());
    }

    #[test]
    fn a_stream_asks_upstream_for_usage() {
        let parsed = request(json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true,
            "stream_options": {"other": true},
        }))
        .unwrap();
        assert!(parsed.stream);
        let forwarded = parsed.forwarded(64);
        assert_eq!(forwarded["stream_options"]["include_usage"], true);
        assert_eq!(forwarded["stream_options"]["other"], true);
        assert!(
            request(json!({
                "model": "m",
                "messages": [{"role": "user", "content": "hi"}],
                "stream": "yes",
            }))
            .is_err()
        );
    }

    #[test]
    fn null_counts_as_absent() {
        let parsed = request(json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": null,
            "stream": null,
        }))
        .unwrap();
        assert_eq!(parsed.max_tokens, None);
        assert!(!parsed.stream);
    }

    #[test]
    fn reads_the_callers_attribution() {
        let parsed = request(json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "user": "u_42",
            "metadata": {"team": "search", "env": "prod"},
            "service_tier": "priority",
        }))
        .unwrap();
        assert_eq!(parsed.attribution.end_user.as_deref(), Some("u_42"));
        assert_eq!(parsed.attribution.service_tier.as_deref(), Some("priority"));
        assert_eq!(
            parsed.attribution.tags.get("team").map(String::as_str),
            Some("search")
        );
        // The fields are recorded, not consumed: upstream sees them unchanged.
        let forwarded = parsed.forwarded(64);
        assert_eq!(forwarded["user"], "u_42");
        assert_eq!(forwarded["metadata"]["team"], "search");
        assert_eq!(forwarded["service_tier"], "priority");
    }

    #[test]
    fn refuses_attribution_past_its_bounds() {
        for (field, extra) in [
            ("user", json!({"user": "u".repeat(129)})),
            ("user", json!({"user": 42})),
            ("metadata", json!({"metadata": {"k": "v".repeat(65)}})),
            ("metadata", json!({"metadata": {"k": 1}})),
            ("metadata", json!({"metadata": "x"})),
            ("metadata", json!({"metadata": {"": "v"}})),
            ("service_tier", json!({"service_tier": "s".repeat(65)})),
            ("service_tier", json!({"service_tier": true})),
        ] {
            let mut body = json!({
                "model": "m",
                "messages": [{"role": "user", "content": "hi"}],
            });
            body.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            let error = request(body).expect_err("out of bounds");
            let GatewayError::Invalid { param, .. } = error else {
                panic!("expected an invalid-request error");
            };
            assert_eq!(param, Some(field), "{field}");
        }
        // Eleven tags is one too many.
        let metadata: Map<String, Value> = (0..11).map(|i| (format!("k{i}"), json!("v"))).collect();
        assert!(
            request(json!({
                "model": "m",
                "messages": [{"role": "user", "content": "hi"}],
                "metadata": metadata,
            }))
            .is_err()
        );
    }

    #[test]
    fn reads_an_anthropic_request() {
        let parsed = messages_request(json!({
            "model": "claude-x",
            "system": "be brief",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 64,
            "metadata": {"user_id": "u_42"},
            "temperature": 0.2,
        }))
        .unwrap();
        assert_eq!(parsed.model, "claude-x");
        // `system` counts toward the input bound too.
        assert_eq!(parsed.texts, ["be brief", "hi"]);
        assert_eq!(parsed.max_tokens, Some(64));
        assert_eq!(parsed.attribution.end_user.as_deref(), Some("u_42"));
        let forwarded = parsed.forwarded(64);
        assert_eq!(forwarded["max_tokens"], 64);
        assert_eq!(forwarded["temperature"], 0.2);
        // Anthropic reports usage on every stream; no stream_options is added.
        assert!(forwarded.get("stream_options").is_none());
    }

    #[test]
    fn anthropic_blocks_contribute_their_computable_text() {
        let parsed = messages_request(json!({
            "model": "m",
            "system": [{"type": "text", "text": "sys"}],
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "hi"},
                    {"type": "tool_use", "name": "lookup", "input": {"q": "x"}},
                ]},
                {"role": "assistant", "content": null},
            ],
        }))
        .unwrap();
        // The text block contributes its text; the tool block its serialized
        // JSON — a computable bound for whatever it carries.
        let tool_use = json!({"type": "tool_use", "name": "lookup", "input": {"q": "x"}});
        assert_eq!(parsed.texts, ["sys", "hi", &tool_use.to_string()]);
    }

    #[test]
    fn an_anthropic_media_block_is_refused() {
        let error = messages_request(json!({
            "model": "m",
            "messages": [{"role": "user", "content": [
                {"type": "image", "source": {"type": "base64", "data": "…"}},
            ]}],
        }))
        .expect_err("an image has no computable input bound");
        let GatewayError::Invalid { message, param } = error else {
            panic!("expected an invalid-request error");
        };
        assert_eq!(param, Some("messages"));
        assert!(message.contains("media block"), "{message}");
    }

    #[test]
    fn anthropic_refusals_and_bounds() {
        for body in [
            json!({"model": "m"}),
            json!({"model": "m", "messages": []}),
            json!({"model": "m", "messages": [{"role": "user", "content": {"x": 1}}]}),
            json!({"model": "m", "system": 7, "messages": [{"content": "hi"}]}),
            json!({"model": "m", "messages": [{"content": "hi"}], "max_tokens": "a lot"}),
            json!({"model": "m", "messages": [{"content": "hi"}], "metadata": {"user_id": "u".repeat(129)}}),
        ] {
            assert!(messages_request(body).is_err());
        }
        // `stream_options` sent on this surface is not ours to strip — but
        // `stream: null` still counts as false.
        let parsed = messages_request(json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": null,
            "max_tokens": null,
        }))
        .unwrap();
        assert!(!parsed.stream);
        assert_eq!(parsed.max_tokens, None);
    }
}
