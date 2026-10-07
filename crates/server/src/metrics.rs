//! Prometheus telemetry: one recorder per app, scraped at `GET /metrics`.
//!
//! The recorder lives in [`AppState`] rather than the `metrics` facade's global slot: every
//! `app()` gets a registry of its own, so a test can drive a turn and read back the exact
//! counts it caused — a shared registry would leak one test's traffic into another's scrape.
//! Instrumentation is at the call site, not on the billing broadcast: the broadcast is lossy
//! under lag and the sweeper never publishes, so each counter is written where the event
//! happens.
//!
//! Label values only ever carry bounded vocabularies: route patterns, status classes,
//! settlement kinds, claim outcomes. Never a request id, an organization or a key — a label
//! that grows per caller would be a cardinality leak the scrape cannot put back.

use std::sync::Arc;
use std::time::Instant;

use axum::extract::{MatchedPath, Request, State};
use axum::http::header::CONTENT_TYPE;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use metrics::Recorder as _;
use metrics::{Key, KeyName, Label, Level, Metadata};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle, PrometheusRecorder};

use crate::AppState;

/// Every request the router answered, labelled by method, matched route pattern and status.
const HTTP_REQUESTS: &str = "oxsum_http_requests_total";
/// Time from a request reaching the router to its response head, per route.
const HTTP_DURATION: &str = "oxsum_http_request_duration_seconds";
/// Time from sending a turn upstream to its response head — `unreachable` and `error` count
/// too, so an upstream outage is visible in the latency picture it is causing.
const UPSTREAM_RESPONSE: &str = "oxsum_upstream_response_seconds";
/// A turn's whole life, hold to settlement.
const GATEWAY_TURN: &str = "oxsum_gateway_turn_seconds";
/// Holds taken on the gateway, by channel and model.
const GATEWAY_HOLDS: &str = "oxsum_gateway_holds_total";
/// Settlements written, by kind (`usage`, `estimated`, `capped`, `unpriced`, …). A sweep's
/// settle does not pass here — it has its own counter, [`HOLDS_SWEPT`].
const GATEWAY_SETTLEMENTS: &str = "oxsum_gateway_settlements_total";
/// Minor units charged across all settlements.
const GATEWAY_CHARGED: &str = "oxsum_gateway_charged_minor_total";
/// Settled tokens, by `direction` (`input`/`output`).
const GATEWAY_TOKENS: &str = "oxsum_gateway_tokens_total";
/// Requests the rate limiter refused, by `surface` (`gateway` or `api`).
const RATE_LIMIT_REJECTED: &str = "oxsum_rate_limit_rejections_total";
/// `Idempotency-Key` claims on the gateway, by `outcome`
/// (`fresh`, `replay`, `in_flight`, `mismatch`).
const IDEMPOTENCY_CLAIMS: &str = "oxsum_idempotency_claims_total";
/// Stale holds the sweeper released, summed over passes.
const HOLDS_SWEPT: &str = "oxsum_holds_swept_total";
/// Holds that crossed into the dead-letter state — transitions, not retries.
const HOLDS_DEAD_LETTERED: &str = "oxsum_holds_dead_lettered_total";
/// Watch rows open right now — refreshed on each scrape, never counted up and down.
const OPEN_HOLDS: &str = "oxsum_open_holds";
/// Dead-lettered watch rows right now — refreshed on each scrape like [`OPEN_HOLDS`].
const DEAD_HOLDS: &str = "oxsum_dead_holds";
/// Webhook deliveries attempted, by `outcome` (`delivered`/`retried`/`failed`).
const WEBHOOK_DELIVERIES: &str = "oxsum_webhook_deliveries_total";
/// The shared connection pool, by `state` (`open`/`idle`) — refreshed on each scrape.
const POOL_CONNECTIONS: &str = "oxsum_db_pool_connections";

/// The per-app metric registry: a Prometheus recorder plus the handle that renders it.
///
/// `Clone` is free — both halves are `Arc` inside — so the gateway's `Turn`, the routes and
/// the sweeper all record into the same registry `/metrics` renders.
#[derive(Clone)]
pub struct Metrics {
    recorder: Arc<PrometheusRecorder>,
    handle: PrometheusHandle,
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

impl Metrics {
    /// A fresh registry, with every series described so the first scrape carries HELP lines.
    #[must_use]
    pub fn new() -> Self {
        let recorder = Arc::new(PrometheusBuilder::new().build_recorder());
        let metrics = Self {
            handle: recorder.handle(),
            recorder,
        };
        metrics.describe();
        metrics
    }

    /// Adds `value` to the counter `name` under `labels`.
    pub(crate) fn count(&self, name: &'static str, value: u64, labels: &[(&'static str, String)]) {
        self.recorder
            .register_counter(&key(name, labels), &metadata())
            .increment(value);
    }

    /// Records `seconds` on the histogram `name` under `labels`.
    pub(crate) fn observe(
        &self,
        name: &'static str,
        seconds: f64,
        labels: &[(&'static str, String)],
    ) {
        self.recorder
            .register_histogram(&key(name, labels), &metadata())
            .record(seconds);
    }

    /// Sets the gauge `name` to `value` under `labels`.
    pub(crate) fn set_gauge(
        &self,
        name: &'static str,
        value: f64,
        labels: &[(&'static str, String)],
    ) {
        self.recorder
            .register_gauge(&key(name, labels), &metadata())
            .set(value);
    }

    /// The whole registry in Prometheus exposition format.
    pub(crate) fn render(&self) -> String {
        self.handle.render()
    }

    /// What the sweeper records after a pass — `main`'s one instrumentation call, which is
    /// why it is a method rather than a module function the binary crate cannot reach.
    pub fn swept(&self, released: u64) {
        self.count(HOLDS_SWEPT, released, &[]);
    }

    /// The sweeper's dead-letter transitions for a pass — `main`'s second
    /// instrumentation call, beside [`Self::swept`].
    pub fn dead_lettered(&self, holds: u64) {
        self.count(HOLDS_DEAD_LETTERED, holds, &[]);
    }

    /// The HELP text of every series, so a scrape explains itself.
    fn describe(&self) {
        const DESCRIPTIONS: &[(&str, &str)] = &[
            (
                HTTP_REQUESTS,
                "HTTP requests answered, by method, route pattern and status.",
            ),
            (
                HTTP_DURATION,
                "Seconds from a request reaching the router to its response head.",
            ),
            (
                UPSTREAM_RESPONSE,
                "Seconds from sending a turn upstream to its response head.",
            ),
            (
                GATEWAY_TURN,
                "Seconds a gateway turn lived, hold to settlement.",
            ),
            (
                GATEWAY_HOLDS,
                "Holds taken on the gateway, by channel and model.",
            ),
            (
                GATEWAY_SETTLEMENTS,
                "Settlements written by gateway turns, by settlement kind.",
            ),
            (GATEWAY_CHARGED, "Minor units charged across settled turns."),
            (GATEWAY_TOKENS, "Settled tokens, by direction."),
            (
                RATE_LIMIT_REJECTED,
                "Requests the per-key rate limiter refused, by surface.",
            ),
            (
                IDEMPOTENCY_CLAIMS,
                "Idempotency-Key claims on the gateway, by outcome.",
            ),
            (HOLDS_SWEPT, "Stale holds the sweeper released."),
            (
                HOLDS_DEAD_LETTERED,
                "Holds whose sweep failures dead-lettered them.",
            ),
            (
                WEBHOOK_DELIVERIES,
                "Webhook deliveries attempted, by outcome.",
            ),
        ];
        for (name, help) in DESCRIPTIONS {
            self.recorder
                .describe_counter(KeyName::from(*name), None, (*help).into());
        }
        for (name, help) in [
            (OPEN_HOLDS, "Hold watch rows currently open."),
            (
                DEAD_HOLDS,
                "Hold watch rows dead-lettered by repeated sweep failures.",
            ),
            (
                POOL_CONNECTIONS,
                "Shared connection pool connections, by state.",
            ),
        ] {
            self.recorder
                .describe_gauge(KeyName::from(name), None, help.into());
        }
    }
}

/// `GET /metrics`: the exposition document, behind the same operator token as the admin
/// surface. The gauges that read the database are refreshed here — a scrape asks, so a quiet
/// deployment does not pay a timer for numbers nobody is reading. A failed gauge refresh is
/// logged and skipped: a metrics endpoint that 500s because one count failed serves nothing.
pub(crate) async fn scrape(State(state): State<AppState>) -> Response {
    let pool = state.db.pool();
    state.metrics.set_gauge(
        POOL_CONNECTIONS,
        f64::from(pool.size()),
        &[("state", "open".to_owned())],
    );
    state.metrics.set_gauge(
        POOL_CONNECTIONS,
        pool.num_idle() as f64,
        &[("state", "idle".to_owned())],
    );
    match state.db.open_hold_count().await {
        Ok(open) => state.metrics.set_gauge(OPEN_HOLDS, open as f64, &[]),
        Err(error) => {
            tracing::error!(%error, "the open-holds gauge could not be refreshed");
        }
    }
    match state.db.dead_hold_count().await {
        Ok(dead) => state.metrics.set_gauge(DEAD_HOLDS, dead as f64, &[]),
        Err(error) => {
            tracing::error!(%error, "the dead-holds gauge could not be refreshed");
        }
    }
    (
        [(CONTENT_TYPE, "text/plain; version=0.0.4")],
        state.metrics.render(),
    )
        .into_response()
}

/// The router-edge middleware: one count and one duration per answered request.
///
/// The label is the matched route *pattern* (`/api/v1/org/keys/{key_id}`), never the
/// concrete path — a per-id path would be an unbounded label. Unrouted requests count under
/// `unmatched`.
pub(crate) async fn track(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let method = request.method().as_str().to_owned();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|matched| matched.as_str().to_owned())
        .unwrap_or_else(|| "unmatched".to_owned());
    let started = Instant::now();
    let response = next.run(request).await;
    let elapsed = started.elapsed().as_secs_f64();
    state.metrics.observe(
        HTTP_DURATION,
        elapsed,
        &[("method", method.clone()), ("route", route.clone())],
    );
    state.metrics.count(
        HTTP_REQUESTS,
        1,
        &[
            ("method", method),
            ("route", route),
            ("status", response.status().as_u16().to_string()),
        ],
    );
    response
}

/// What the gateway records when a hold lands.
pub(crate) fn hold(metrics: &Metrics, channel: &str, model: &str) {
    metrics.count(
        GATEWAY_HOLDS,
        1,
        &[("channel", channel.to_owned()), ("model", model.to_owned())],
    );
}

/// What the gateway records when the rate limiter refuses — `surface` says which entry point.
pub(crate) fn rate_limited(metrics: &Metrics, surface: &'static str) {
    metrics.count(RATE_LIMIT_REJECTED, 1, &[("surface", surface.to_owned())]);
}

/// What the gateway records for an `Idempotency-Key` claim's outcome.
pub(crate) fn claim(metrics: &Metrics, outcome: &'static str) {
    metrics.count(IDEMPOTENCY_CLAIMS, 1, &[("outcome", outcome.to_owned())]);
}

/// What the webhook worker records after a delivery attempt — `delivered`,
/// `retried` (inside the budget) or `failed` (the budget ran out).
pub(crate) fn webhook_delivery(metrics: &Metrics, outcome: &'static str) {
    metrics.count(WEBHOOK_DELIVERIES, 1, &[("outcome", outcome.to_owned())]);
}

/// What the relay records at settlement: the kind, the charge, the tokens and the turn's age.
pub(crate) fn settled(
    metrics: &Metrics,
    kind: oxsum_core::SettlementKind,
    charged_minor: i64,
    input_tokens: i64,
    output_tokens: i64,
    turn_seconds: f64,
) {
    metrics.count(
        GATEWAY_SETTLEMENTS,
        1,
        &[("kind", kind.as_str().to_owned())],
    );
    metrics.count(
        GATEWAY_CHARGED,
        u64::try_from(charged_minor).unwrap_or(0),
        &[],
    );
    for (direction, tokens) in [("input", input_tokens), ("output", output_tokens)] {
        metrics.count(
            GATEWAY_TOKENS,
            u64::try_from(tokens).unwrap_or(0),
            &[("direction", direction.to_owned())],
        );
    }
    metrics.observe(GATEWAY_TURN, turn_seconds, &[]);
}

/// What the gateway records for the upstream call's time to response head.
pub(crate) fn upstream_response(metrics: &Metrics, result: &'static str, seconds: f64) {
    metrics.observe(UPSTREAM_RESPONSE, seconds, &[("result", result.to_owned())]);
}

/// The key a series registers under: name plus its bounded labels.
fn key(name: &'static str, labels: &[(&'static str, String)]) -> Key {
    Key::from_parts(
        name,
        labels
            .iter()
            .map(|(label, value)| Label::new(*label, value.clone()))
            .collect::<Vec<_>>(),
    )
}

/// The metadata every registration carries: no module path worth filtering on.
fn metadata() -> Metadata<'static> {
    Metadata::new("", Level::INFO, None)
}
