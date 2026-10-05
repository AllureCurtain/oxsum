//! Relaying an upstream stream, and settling the turn however it ends.
//!
//! Three ways a turn ends, and all three settle:
//!
//! - Upstream finishes. The usage it reported is the charge, or a local estimate when it reported
//!   none.
//! - Upstream fails mid-stream. What was forwarded is estimated; the rest is not charged.
//! - The client goes away. hyper drops the response body, which drops the generator below, which
//!   drops the upstream response — reqwest's only way to cancel a call — and the turn settles from
//!   what had been forwarded, as `client_cancelled`, or as the finished turn it already was when the
//!   client left after upstream had ended.
//!
//! The last one is why the turn owns its settlement instead of the handler doing it: a dropped
//! future runs no code after the drop point, so the promise to settle travels with the data. It is
//! also why the settlement outlives the response body: the plan is only given up once an entry has
//! been attempted, so a client that disconnects *during* the append is settled by the drop instead
//! of leaving its hold outstanding. An OpenAI SDK client does exactly that, closing the connection as
//! soon as it reads the terminator.

use std::sync::Arc;

use axum::body::Bytes;
use futures_core::Stream;
use oxsum_core::{
    Attribution, Db, Price, Receipt, Serving, Settlement, SettlementKind, UsageAdapter,
    UsageRecord, UsageRow, Wallet, WalletError, entry_id_for, estimate_tokens, settlement_key_for,
};
use serde_json::Value;
use tokio::sync::broadcast;

use crate::billing::BillingEvent;
use crate::today;

/// How much forwarded text is kept for the local estimate.
///
/// A turn that streams longer than this is estimated from its first 256 KiB. That undercounts the
/// output, and an undercount is the platform's cost rather than the user's (product.md): the
/// alternative is holding a whole completion in memory to price it.
const ESTIMATE_WINDOW: usize = 256 * 1024;

/// A progress event goes out at most every this many forwarded answer characters.
///
/// The dashboard shows live turns; per-chunk events would be a push per upstream packet.
const PROGRESS_EVERY_CHARS: usize = 1024;

/// What a turn is charged for.
#[derive(Debug, Clone)]
pub enum Charge {
    /// Nothing: upstream failed before emitting anything, or could not be reached at all.
    Nothing,
    /// Upstream's own counts, normalized. Boxed: the record is wide and the other
    /// variants carry nothing.
    Usage(Box<UsageRecord>),
    /// A local estimate over the input and what was forwarded.
    Estimated,
}

/// One frozen turn, and the promise to settle it.
pub struct Turn {
    /// Taken by `settle`, or by `Drop` when the client goes away, so it settles exactly once.
    plan: Option<Plan>,
}

/// Everything a settlement needs, including the text the estimate is computed from.
struct Plan {
    wallet: Arc<Wallet>,
    /// The watch row this hold was noted under, cleared once the turn settles: the sweeper only
    /// looks at rows, so a settled turn must leave none behind.
    db: Db,
    /// `req-<id>:hold`: the key the hold was taken under, which the settlement names.
    hold_key: String,
    /// The request id, as the description and the response header carry it.
    request: String,
    /// The organization whose ledger holds the freeze, as its ledger tenant id: billing
    /// events are filtered on it, so one organization's turns never reach another's
    /// dashboard.
    tenant_id: String,
    /// Where billing progress goes: the dashboard's live holds section.
    billing: broadcast::Sender<BillingEvent>,
    /// The channel that served the request, and the price version it was priced by. Both go into the
    /// settlement, so a bill says which version priced it and not only at what price.
    channel: String,
    version: i64,
    model: String,
    price: Price,
    freeze: i64,
    /// The key that took the hold: what the usage row records as its payer.
    key_id: uuid::Uuid,
    /// The caller's attribution on the turn, merged into the settled usage record.
    attribution: Attribution,
    /// The protocol adapter that reads this channel's usage reports.
    adapter: &'static dyn UsageAdapter,
    /// The input texts, for the estimate.
    texts: Vec<String>,
    /// What upstream said and what it emitted, for the estimate.
    frames: Frames,
    /// How many forwarded answer characters the last progress event covered: a new one
    /// goes out every [`PROGRESS_EVERY_CHARS`].
    progress_chars: usize,
    /// Set once upstream has ended and only the local write is left. A client that goes away then
    /// has not cut the turn short: the turn is billed as the finished turn it is.
    finished: bool,
}

/// The SSE scanner: chunks in, usage and answer text out.
///
/// It knows nothing about money or wallets, which is what makes it testable on its own.
#[derive(Debug, Default)]
struct Frames {
    /// Bytes of the frame currently being assembled. A frame can be split across chunks.
    buffer: Vec<u8>,
    /// The answer text forwarded so far, for the estimate, capped at [`ESTIMATE_WINDOW`].
    output: String,
    /// Upstream's report, once a chunk carried a usable one.
    usage: Option<UsageRecord>,
}

impl Frames {
    /// Reads one chunk and reports whether it contained the stream's own terminator.
    /// `adapter` reads each frame's usage report in the channel's own protocol.
    fn observe(&mut self, chunk: &[u8], adapter: &dyn UsageAdapter) -> bool {
        self.buffer.extend_from_slice(chunk);
        let mut terminated = false;
        // SSE is line-delimited, so only whole lines are parsed and a partial line waits.
        while let Some(newline) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=newline).collect();
            let line = String::from_utf8_lossy(&line[..line.len() - 1]);
            let Some(payload) = line.strip_prefix("data:") else {
                continue;
            };
            let payload = payload.trim();
            if payload == "[DONE]" {
                terminated = true;
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(payload) else {
                continue;
            };
            if let Some(usage) = adapter.usage(&value) {
                self.usage = Some(usage);
            }
            let text = completion_text(&value);
            self.push_text(&text);
        }
        terminated
    }

    /// Keeps answer text for the estimate, up to the window.
    fn push_text(&mut self, text: &str) {
        if text.is_empty() || self.output.len() + text.len() > ESTIMATE_WINDOW {
            return;
        }
        self.output.push_str(text);
    }
}

impl Turn {
    /// Starts a turn that has already been frozen, priced by the channel and version it started on.
    // Eleven arguments because a turn needs the whole settlement context; splitting the
    // constructor would just move the list somewhere else.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        db: Db,
        wallet: Arc<Wallet>,
        request_id: &str,
        model: &str,
        serving: &Serving,
        freeze: i64,
        texts: Vec<String>,
        tenant_id: &str,
        billing: broadcast::Sender<BillingEvent>,
        key_id: uuid::Uuid,
        attribution: Attribution,
        adapter: &'static dyn UsageAdapter,
    ) -> Self {
        Self {
            plan: Some(Plan {
                db,
                wallet,
                hold_key: format!("req-{request_id}:hold"),
                request: request_id.to_owned(),
                tenant_id: tenant_id.to_owned(),
                billing,
                channel: serving.channel.clone(),
                version: serving.version,
                model: model.to_owned(),
                price: serving.price.clone(),
                freeze,
                key_id,
                attribution,
                adapter,
                texts,
                frames: Frames::default(),
                finished: false,
                progress_chars: 0,
            }),
        }
    }

    /// What this turn is charged for if upstream just ended: its usage, or an estimate.
    #[must_use]
    pub fn closing(&self) -> (SettlementKind, Charge) {
        match self
            .plan
            .as_ref()
            .and_then(|plan| plan.frames.usage.clone())
        {
            Some(usage) => (SettlementKind::Usage, Charge::Usage(Box::new(usage))),
            None => (SettlementKind::Estimated, Charge::Estimated),
        }
    }

    /// Adds text to the estimate window, for a body read whole rather than streamed.
    pub fn note_text(&mut self, text: &str) {
        if let Some(plan) = self.plan.as_mut() {
            plan.frames.push_text(text);
        }
    }

    /// Records upstream's usage report from a body read whole. A streamed turn gets the same thing
    /// out of [`observe`](Self::observe), from the chunk that carries it.
    pub fn note_usage(&mut self, usage: UsageRecord) {
        if let Some(plan) = self.plan.as_mut() {
            plan.frames.usage = Some(usage);
        }
    }

    /// Reads one streamed chunk, reports whether it carried the stream's terminator,
    /// and publishes billing progress as the answer grows.
    pub fn observe(&mut self, chunk: &[u8]) -> bool {
        let Some(plan) = self.plan.as_mut() else {
            return false;
        };
        let terminated = plan.frames.observe(chunk, plan.adapter);
        // Best-effort: a dashboard that is not listening misses a progress tick, and the
        // settlement at the end still carries the final charge.
        let output_chars = plan.frames.output.len();
        if output_chars >= plan.progress_chars + PROGRESS_EVERY_CHARS {
            plan.progress_chars = output_chars;
            let _ = plan.billing.send(BillingEvent::TurnProgress {
                tenant_id: plan.tenant_id.clone(),
                request_id: plan.request.clone(),
                output_chars,
            });
        }
        terminated
    }

    /// Notes that upstream has ended: the turn is finished, and only the local write is left. A
    /// client that goes away from here on has not cut the turn short, and `Drop` bills it as the
    /// finished turn it is.
    pub fn note_upstream_end(&mut self) {
        if let Some(plan) = self.plan.as_mut() {
            plan.finished = true;
        }
    }

    /// Writes the settlement for this turn. Called once; later calls do nothing.
    ///
    /// The write runs in a task of its own and is awaited here, so that nothing can cancel a
    /// settlement that has begun. hyper drops this future — and the plan in it — when the client
    /// hangs up, which over a real socket happens while the entry is being appended; a cancelled
    /// append leaves the freeze reserved with nothing left to release it, and a retried write behind
    /// it waits on the transaction the abandoned one still holds. An OpenAI SDK client closes the
    /// connection the moment it reads the terminator, so this is the ordinary case, not an edge one.
    ///
    /// A settlement that fails is logged, not returned: the response may already be on its way, and
    /// the hold it would have released stays outstanding for the sweeper (issue #13) instead of
    /// turning into a 500 after upstream has already answered.
    pub async fn settle(&mut self, kind: SettlementKind, charge: Charge) {
        let Some(mut plan) = self.plan.take() else {
            return;
        };
        let request = plan.request.clone();
        match tokio::spawn(async move { plan.write(kind, charge).await }).await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                tracing::error!(%error, %request, "settling a gateway turn failed");
            }
            Err(error) => {
                tracing::error!(%error, %request, "the settlement task did not finish");
            }
        }
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        let Some(mut plan) = self.plan.take() else {
            return;
        };
        // Reaching here means the stream was dropped before it settled: the client disconnected, or
        // the process is shutting down. The upstream call went with this struct, and what was
        // forwarded before that is what the user received. The one case that is not a cut-short turn
        // is a client that goes away after upstream has already ended — the mainstream OpenAI SDK
        // does exactly that, closing the moment it reads the terminator — and that turn is billed as
        // the finished turn it is, with upstream's own counts when it reported them.
        let usage = plan.frames.usage.take().map(Box::new);
        let (kind, charge) = if plan.finished {
            match usage {
                Some(usage) => (SettlementKind::Usage, Charge::Usage(usage)),
                None => (SettlementKind::Estimated, Charge::Estimated),
            }
        } else {
            // Upstream's counts when it already reported them, and an estimate of what was forwarded
            // otherwise: the kind says the client left, and it should still not pay for an estimate
            // when upstream had already counted the turn.
            let charge = match usage {
                Some(usage) => Charge::Usage(usage),
                None => Charge::Estimated,
            };
            (SettlementKind::ClientCancelled, charge)
        };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::error!(request = %plan.request, "no runtime to settle a cancelled turn");
            return;
        };
        handle.spawn(async move {
            if let Err(error) = plan.write(kind, charge).await {
                tracing::error!(%error, request = %plan.request, "settling a turn dropped mid-flight failed");
            }
        });
    }
}

impl Plan {
    /// Charges the turn and writes the settlement entry.
    async fn write(
        &mut self,
        kind: SettlementKind,
        charge: Charge,
    ) -> Result<Receipt, WalletError> {
        let mut usage = match charge {
            Charge::Nothing => UsageRecord::tokens(0, 0)?,
            Charge::Usage(usage) => *usage,
            Charge::Estimated => {
                let input: Vec<&str> = self.texts.iter().map(String::as_str).collect();
                let output = estimate_tokens(&[self.frames.output.as_str()]);
                UsageRecord::tokens(estimate_tokens(&input), output)?
            }
        };
        // The caller's attribution rides whatever the turn was charged for: it is
        // request-scoped, not upstream-reported.
        usage.end_user = self.attribution.end_user.clone();
        usage.tags = self.attribution.tags.clone();
        usage.service_tier = self.attribution.service_tier.clone();
        // The user never pays more than the freeze: that is the gateway's promise (product.md). A
        // usage report above it is charged at the freeze and recorded as `capped`, an anomaly for
        // the admin page rather than a silent loss. A turn upstream never served owes no flat fee
        // either — `billable` is whether the request ran.
        let billable = !matches!(
            kind,
            SettlementKind::UpstreamError | SettlementKind::UpstreamUnreachable
        );
        let itemized = self.price.itemize(&usage, billable)?;
        let cost = itemized.total_minor()?;
        // Fail closed on what the price book cannot cover: a usage report naming a
        // dimension outside every price set settles for the computable part and is
        // recorded `unpriced` — the anomaly the admin page reviews — never billed at
        // zero or folded into a rate the dimension does not belong to (issue #110).
        let unpriced = self.price.unpriced_dimensions(&usage);
        let (kind, charged) = if !unpriced.is_empty() {
            tracing::error!(request = %self.request, model = %self.model, ?unpriced,
                "upstream's usage carried dimensions the price book cannot bill");
            (SettlementKind::Unpriced, cost.min(self.freeze))
        } else if cost > self.freeze {
            (SettlementKind::Capped, self.freeze)
        } else {
            (kind, cost)
        };
        let settlement = Settlement {
            request: &self.request,
            channel: &self.channel,
            model: &self.model,
            price_version: self.version,
            kind,
            usage: &usage,
            lines: &itemized.lines,
            matched_rule: itemized.matched_rule.as_ref(),
            charged,
            freeze: self.freeze,
        };
        let description = settlement.description()?;
        let outcome = self
            .wallet
            .settle(&self.hold_key, &description, charged, today())
            .await;
        match &outcome {
            Ok(_) => {
                // The turn's usage row rides beside the settlement it describes: the ledger
                // entry is the source of truth, and a failed write is drift for the
                // reconciler to report, not a reason to retry the charge.
                let row = UsageRow {
                    request_id: self.request.clone(),
                    tenant_id: self.tenant_id.clone(),
                    key_id: Some(self.key_id),
                    model: self.model.clone(),
                    channel: self.channel.clone(),
                    price_version: self.version,
                    kind,
                    entry_id: *entry_id_for(&settlement_key_for(&self.hold_key)).as_uuid(),
                    usage: usage.clone(),
                    charged_minor: charged,
                    freeze_minor: self.freeze,
                    // The platform's own cost for the same usage, under the set the
                    // turn priced with — `None` when it carries no `upstream`
                    // block, which is untracked rather than free (issue #112).
                    upstream_cost_minor: self.price.upstream_cost(&usage, billable).unwrap_or_else(
                        |error| {
                            tracing::error!(%error, request = %self.request,
                                "pricing upstream's cost failed; the row records untracked");
                            None
                        },
                    ),
                };
                if let Err(error) = self.db.record_usage(&row).await {
                    tracing::error!(%error, request = %self.request,
                        "recording the settled turn's usage row failed");
                }
                if let Err(error) = self.db.clear_open_hold(&self.hold_key).await {
                    tracing::error!(%error, request = %self.request,
                        "clearing the settled hold's watch row failed");
                }
            }
            // The conflict arm is the race the derived settlement key decides: the sweeper
            // settled it first and wrote its own record, so this turn must not write one.
            // `HoldNotFound` is the hold being gone another way. Either way there is nothing
            // left to watch.
            Err(WalletError::Conflict(_)) | Err(WalletError::HoldNotFound(_)) => {
                if let Err(error) = self.db.clear_open_hold(&self.hold_key).await {
                    tracing::error!(%error, request = %self.request,
                        "clearing the settled hold's watch row failed");
                }
            }
            // Any other failure leaves the row watched: the settlement did not land, and the
            // sweeper retries what the turn could not finish.
            Err(_) => {}
        }
        // The turn is over however the write went: the dashboard stops showing it live.
        // Best-effort, like the progress ticks — the ledger, not this event, is the bill.
        let _ = self.billing.send(BillingEvent::TurnSettled {
            tenant_id: self.tenant_id.clone(),
            request_id: self.request.clone(),
            charged_minor: charged,
            kind,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
        });
        outcome
    }
}

/// Relays `upstream` to the client and settles the turn when the stream ends.
pub fn stream(
    mut turn: Turn,
    mut upstream: reqwest::Response,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static {
    async_stream::stream! {
        let mut terminated = false;
        loop {
            match upstream.chunk().await {
                Ok(Some(chunk)) => {
                    terminated |= turn.observe(&chunk);
                    yield Ok(chunk);
                }
                Ok(None) => break,
                Err(error) => {
                    // Upstream dropped mid-stream. Whatever was forwarded already went out; the turn
                    // settles on an estimate of it, and the client is closed off below.
                    tracing::warn!(%error, "the upstream stream failed mid-flight");
                    break;
                }
            }
        }
        // Upstream is done, whatever the client does next: what ends here is a finished turn.
        turn.note_upstream_end();
        let (kind, charge) = turn.closing();
        turn.settle(kind, charge).await;
        if !terminated {
            // Upstream ended without its own terminator, so an SSE reader would wait for an event
            // that is never coming. Close the stream for it.
            yield Ok(Bytes::from_static(b"data: [DONE]\n\n"));
        }
    }
}

/// The answer text in a chunk or a body: the delta of a streamed chunk, or the message of a whole
/// completion. Used only for the local estimate.
pub(crate) fn completion_text(value: &Value) -> String {
    let mut text = String::new();
    let Some(choices) = value.get("choices").and_then(Value::as_array) else {
        return text;
    };
    for choice in choices {
        let part = choice
            .get("delta")
            .and_then(|delta| delta.get("content"))
            .or_else(|| {
                choice
                    .get("message")
                    .and_then(|message| message.get("content"))
            });
        if let Some(Value::String(part)) = part {
            text.push_str(part);
        }
    }
    text
}

#[cfg(test)]
mod tests {
    // The tests may unwrap: a panic here is a failing test, which is what a test is for.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use serde_json::json;

    /// Every frame these tests feed the scanner is in the one protocol this build
    /// knows.
    fn adapter() -> &'static dyn UsageAdapter {
        oxsum_core::adapter_for(oxsum_core::OPENAI).expect("the OpenAI adapter")
    }

    #[test]
    fn usage_is_read_from_a_final_chunk_and_ignored_when_absent() {
        let adapter = adapter();
        let mut frames = Frames::default();
        frames.observe(b"data: {\"choices\": [], \"usage\": null}\n\n", adapter);
        frames.observe(b"data: {\"usage\": {}}\n\n", adapter);
        assert!(frames.usage.is_none());
        frames.observe(
            b"data: {\"usage\": {\"prompt_tokens\": 11, \"completion_tokens\": 7}}\n\n",
            adapter,
        );
        let usage = frames.usage.expect("a chunk with counts carries usage");
        assert_eq!(usage.input_tokens, 11);
        assert_eq!(usage.output_tokens, 7);
    }

    #[test]
    fn the_answer_text_is_read_from_a_delta_or_a_message() {
        assert_eq!(
            completion_text(&json!({"choices": [{"delta": {"content": "he"}}]})),
            "he"
        );
        assert_eq!(
            completion_text(&json!({"choices": [{"delta": {"content": "llo"}}, {"delta": {}}]})),
            "llo"
        );
        assert_eq!(
            completion_text(&json!({"choices": [{"message": {"content": "whole"}}]})),
            "whole"
        );
        assert_eq!(completion_text(&json!({"choices": []})), "");
        assert_eq!(completion_text(&json!({"error": "no"})), "");
    }

    #[test]
    fn frames_split_across_chunks_are_reassembled() {
        let mut frames = Frames::default();
        // One frame, then the terminator itself cut in two.
        assert!(!frames.observe(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: [DO",
            adapter()
        ));
        assert!(frames.observe(b"NE]\n\n", adapter()));
        assert_eq!(frames.output, "hi");
        assert!(frames.usage.is_none());
    }

    #[test]
    fn a_usage_frame_is_kept_and_garbage_is_ignored() {
        let mut frames = Frames::default();
        frames.observe(b": keep-alive\n\ndata: not json\n\n", adapter());
        assert!(frames.usage.is_none());
        frames.observe(
            b"data: {\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":3}}\n\n",
            adapter(),
        );
        let usage = frames.usage.expect("the usage frame was kept");
        assert_eq!(usage.input_tokens, 2);
        assert_eq!(usage.output_tokens, 3);
    }

    #[test]
    fn the_estimate_window_stops_growing() {
        let mut frames = Frames::default();
        frames.push_text(&"x".repeat(ESTIMATE_WINDOW + 1));
        assert_eq!(frames.output.len(), 0);
        frames.push_text(&"y".repeat(ESTIMATE_WINDOW));
        frames.push_text("more");
        assert_eq!(frames.output.len(), ESTIMATE_WINDOW);
    }
}
