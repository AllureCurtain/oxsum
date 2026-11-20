//! Bounded SSE evidence for the explicitly accepted provider-reported stream policy.
//!
//! This parser is independent of permissive legacy adapters. It forwards complete
//! validated events, never infers missing quantities, and never sums final reports.
use axum::body::Bytes;
use oxsum_core::usage_recovery::ObservedTextUsage;
use serde_json::{Value, json};

const MAX_EVENT_BYTES: usize = 64 * 1024;
const MAX_EVENTS: usize = 1_000_000;

/// One complete provider event. Terminal bytes are held until financial handoff.
pub(super) struct Event {
    pub bytes: Bytes,
    pub done: bool,
    pub text: Option<String>,
}

/// Sanitized state plus a bounded incomplete SSE event.
#[derive(Default)]
pub(super) struct Decoder {
    pending: Vec<u8>,
    events: usize,
    finished: bool,
    done: bool,
    id: Option<String>,
    model: Option<String>,
    pub observed: ObservedTextUsage,
}

impl Decoder {
    /// Consume through one blank-line event boundary. The caller processes it before more bytes.
    pub fn next(&mut self, bytes: &mut &[u8]) -> Result<Option<Event>, &'static str> {
        if self.done {
            *bytes = &[];
            return Ok(None);
        }
        while let Some((&byte, rest)) = bytes.split_first() {
            *bytes = rest;
            if self.pending.len() == MAX_EVENT_BYTES {
                return Err("provider_event_too_large");
            }
            self.pending.push(byte);
            if self.pending.ends_with(b"\n\n") || self.pending.ends_with(b"\r\n\r\n") {
                let raw = std::mem::take(&mut self.pending);
                self.events += 1;
                if self.events > MAX_EVENTS {
                    return Err("provider_event_limit");
                }
                return self.event(raw);
            }
        }
        Ok(None)
    }

    fn event(&mut self, raw: Vec<u8>) -> Result<Option<Event>, &'static str> {
        let text = std::str::from_utf8(&raw).map_err(|_| "invalid_provider_utf8")?;
        let mut data = Vec::new();
        for line in text.lines() {
            if line.is_empty() || line.starts_with(':') {
                continue;
            }
            let Some(value) = line.strip_prefix("data:") else {
                return Err("unsupported_provider_sse_field");
            };
            data.push(value.strip_prefix(' ').unwrap_or(value));
        }
        if data.is_empty() {
            return Ok(Some(Event {
                bytes: raw.into(),
                done: false,
                text: None,
            }));
        }
        let data = data.join("\n");
        if data == "[DONE]" {
            if !self.finished || !self.observed.eligible {
                return Err("provider_done_without_finish");
            }
            self.done = true;
            return Ok(Some(Event {
                bytes: raw.into(),
                done: true,
                text: None,
            }));
        }
        let value: Value = serde_json::from_str(&data).map_err(|_| "invalid_provider_json")?;
        if value.get("object").and_then(Value::as_str) != Some("chat.completion.chunk") {
            return Err("unsupported_provider_chunk");
        }
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| {
                !id.is_empty()
                    && id.len() <= 200
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            })
            .ok_or("invalid_provider_completion_id")?;
        if self.id.as_deref().is_some_and(|old| old != id) {
            return Err("conflicting_provider_completion_id");
        }
        self.id = Some(id.into());
        self.observed.completion_id = Some(id.into());
        if let Some(model) = value.get("model") {
            let model = model
                .as_str()
                .filter(|m| !m.is_empty() && m.len() <= 256)
                .ok_or("invalid_provider_model")?;
            if self.model.as_deref().is_some_and(|old| old != model) {
                return Err("conflicting_provider_model");
            }
            self.model = Some(model.into());
        }
        let choices = value
            .get("choices")
            .and_then(Value::as_array)
            .ok_or("missing_provider_choices")?;
        let mut content = None;
        if choices.is_empty() {
            if !self.finished || value.get("usage").is_none_or(Value::is_null) {
                return Err("unexpected_empty_provider_choices");
            }
        } else {
            if choices.len() != 1 || choices[0].get("index").and_then(Value::as_i64) != Some(0) {
                return Err("unsupported_provider_choices");
            }
            let choice = &choices[0];
            let delta = choice
                .get("delta")
                .and_then(Value::as_object)
                .ok_or("invalid_provider_delta")?;
            for (key, value) in delta {
                match key.as_str() {
                    "content" => {
                        if !value.is_null() {
                            let text = value.as_str().ok_or("unsupported_provider_content")?;
                            if self.finished && !text.is_empty() {
                                return Err("provider_content_after_finish");
                            }
                            if !text.is_empty() {
                                self.observed.eligible = true;
                                content = Some(text.into());
                            }
                        }
                    }
                    "role" if value.as_str() == Some("assistant") => {}
                    "reasoning_content" if value.is_null() || value.as_str() == Some("") => {}
                    "tool_calls" | "function_call" | "audio" | "refusal" if value.is_null() => {}
                    _ => return Err("unsupported_provider_delta_field"),
                }
            }
            match choice.get("finish_reason") {
                None | Some(Value::Null) => {}
                Some(reason)
                    if matches!(reason.as_str(), Some("stop" | "length" | "content_filter")) =>
                {
                    self.finished = true;
                    self.observed.eligible = true;
                }
                _ => return Err("unsupported_provider_finish"),
            }
        }
        if let Some(usage) = value.get("usage").filter(|u| !u.is_null()) {
            if !self.finished || !usage.is_object() {
                return Err("nonfinal_provider_usage");
            }
            let report = ObservedTextUsage::parse(&json!({
                "id":id,"object":"chat.completion",
                "choices":[{"index":0,"message":{"content":""},"finish_reason":"stop"}],
                "usage":usage
            }));
            self.observed.merge(&report);
        }
        Ok(Some(Event {
            bytes: raw.into(),
            done: false,
            text: content,
        }))
    }

    /// Structural ambiguity invalidates all quantities, not the observed existence of execution.
    pub fn invalidate(&mut self, reason: &str) {
        self.observed = ObservedTextUsage {
            invalid: vec![reason.into()],
            completion_id: self.id.clone(),
            ..ObservedTextUsage::default()
        };
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    fn frame(id: &str, choices: Value, usage: Value) -> Vec<u8> {
        format!(
            "data: {}\r\n\r\n",
            json!({"id":id,"object":"chat.completion.chunk","choices":choices,"usage":usage})
        )
        .into_bytes()
    }
    fn consume(decoder: &mut Decoder, input: &[u8]) -> Result<usize, &'static str> {
        let mut remaining = input;
        let mut events = 0;
        while !remaining.is_empty() {
            if decoder.next(&mut remaining)?.is_some() {
                events += 1;
            }
        }
        Ok(events)
    }
    #[test]
    fn split_utf8_duplicate_usage_and_done_are_bounded_and_not_summed() {
        let mut decoder = Decoder::default();
        let text = frame(
            "fabc-123",
            json!([{"index":0,"delta":{"content":"你好"},"finish_reason":null}]),
            Value::Null,
        );
        for byte in text {
            consume(&mut decoder, &[byte]).unwrap();
        }
        consume(
            &mut decoder,
            &frame(
                "fabc-123",
                json!([{"index":0,"delta":{},"finish_reason":"stop"}]),
                Value::Null,
            ),
        )
        .unwrap();
        let usage = frame(
            "fabc-123",
            json!([]),
            json!({"prompt_tokens":9,"completion_tokens":1,"total_tokens":10}),
        );
        consume(&mut decoder, &usage).unwrap();
        consume(&mut decoder, &usage).unwrap();
        consume(&mut decoder, b"data: [DONE]\n\n").unwrap();
        assert!(decoder.done);
        assert_eq!(
            (decoder.observed.input, decoder.observed.output),
            (Some(9), Some(1))
        );
        assert_eq!(decoder.observed.completion_id.as_deref(), Some("fabc-123"));
    }
    #[test]
    fn conflicts_and_unsupported_shapes_cannot_be_billed_as_trusted_usage() {
        let mut d = Decoder::default();
        consume(
            &mut d,
            &frame(
                "x",
                json!([{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}]),
                json!({"prompt_tokens":9,"completion_tokens":1}),
            ),
        )
        .unwrap();
        consume(
            &mut d,
            &frame(
                "x",
                json!([]),
                json!({"prompt_tokens":20,"completion_tokens":1}),
            ),
        )
        .unwrap();
        assert_eq!(d.observed.input, None);
        assert_eq!(d.observed.output, Some(1));
        assert!(
            consume(
                &mut d,
                &frame("other", json!([]), json!({"prompt_tokens":9}))
            )
            .is_err()
        );
        for delta in [
            json!({"tool_calls":[]}),
            json!({"reasoning_content":"secret thought"}),
            json!({"content":42}),
        ] {
            assert!(
                consume(
                    &mut Decoder::default(),
                    &frame(
                        "x",
                        json!([{"index":0,"delta":delta,"finish_reason":null}]),
                        Value::Null
                    )
                )
                .is_err()
            );
        }
        assert!(consume(&mut Decoder::default(), b"data: [DONE]\n\n").is_err());
        assert!(consume(&mut Decoder::default(), &vec![b'x'; MAX_EVENT_BYTES + 1]).is_err());
    }
}
