//! The OpenAI chat request, as far as the gateway reads it.
//!
//! Everything the gateway does not act on is forwarded upstream unchanged, which is what keeps an
//! SDK working here by changing `base_url` only. The fields it does act on are taken out and
//! validated once, so the rest of the request path never has to pick apart JSON.

use serde_json::{Map, Value, json};

use super::error::GatewayError;

/// One parsed chat request: the body to forward, and the parts the freeze depends on.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    /// The caller's body, minus nothing: what is sent upstream is this, with the output ceiling
    /// written into it.
    body: Map<String, Value>,
    /// The model the caller asked for, which must have a configured price.
    pub model: String,
    /// Whether the caller asked for a streamed answer.
    pub stream: bool,
    /// The text of every message, for the input upper bound and the local estimate. A message with
    /// no text (an assistant turn that only calls a tool) contributes none.
    pub texts: Vec<String>,
    /// The output ceiling the caller asked for, if it asked for one.
    pub max_tokens: Option<i64>,
}

impl ChatRequest {
    /// Reads a request body.
    ///
    /// # Errors
    ///
    /// Refuses a body that is not an object, a missing or empty `model`, a missing or empty
    /// `messages`, a non-boolean `stream`, a non-integer output ceiling, and any content that is not
    /// text: v1 prices chat text only, and the freeze is only a promise if the input bound is
    /// computable (product.md).
    pub fn parse(body: Value) -> Result<Self, GatewayError> {
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
            Some(Value::Bool(true)) => true,
            Some(_) => return Err(param("stream", "stream must be a boolean")),
        };
        let texts = texts_of(&body)?;
        let max_tokens = output_ceiling(&body)?;
        Ok(Self {
            body,
            model,
            stream,
            texts,
            max_tokens,
        })
    }

    /// The body to send upstream: the caller's, with the output upper bound written in and usage
    /// reporting switched on for a stream.
    ///
    /// The bound is written as `max_tokens` and any `max_completion_tokens` is dropped, so upstream
    /// sees exactly one ceiling — the one the freeze was computed for. A stream always asks for
    /// usage, because the last chunk is where this gateway gets the numbers it settles against.
    #[must_use]
    pub fn forwarded(&self, output_bound: i64) -> Value {
        let mut body = self.body.clone();
        body.insert("max_tokens".to_owned(), json!(output_bound));
        body.remove("max_completion_tokens");
        if self.stream {
            let mut options = body
                .get("stream_options")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            options.insert("include_usage".to_owned(), Value::Bool(true));
            body.insert("stream_options".to_owned(), Value::Object(options));
        }
        Value::Object(body)
    }
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

    fn request(body: Value) -> Result<ChatRequest, GatewayError> {
        ChatRequest::parse(body)
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
}
