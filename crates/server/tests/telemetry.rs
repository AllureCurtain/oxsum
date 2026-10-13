//! OTLP trace export (issue #224): the wired layer ships a span to the collector
//! the endpoint names, and the exporter speaks OTLP/HTTP protobuf — a stub
//! collector on a loopback port is the whole harness.
//!
//! The test drives `telemetry::layer` under a scoped `with_default` rather than
//! the process-wide `init`: a global subscriber could only be installed once per
//! test binary, and the scoped form exercises the same code — the layer, the
//! batch exporter, the guard's flush — without owning the process.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Mutex;

use axum::Router;
use axum::body::Bytes;
use axum::routing::post;
use tracing_subscriber::layer::SubscriberExt;

/// What the stub collector keeps: the bodies POSTed to `/v1/traces`, in order.
struct Collector {
    bodies: Mutex<Vec<Vec<u8>>>,
}

async fn collect(
    state: axum::extract::State<std::sync::Arc<Collector>>,
    body: Bytes,
) -> &'static str {
    state.bodies.lock().unwrap().push(body.to_vec());
    ""
}

/// A loopback collector: returns its bound port and the received bodies.
async fn collector() -> (u16, std::sync::Arc<Collector>) {
    let state = std::sync::Arc::new(Collector {
        bodies: Mutex::new(Vec::new()),
    });
    let app = Router::new()
        .route("/v1/traces", post(collect))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("binds a port");
    let port = listener.local_addr().expect("the bound address").port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("the stub serves");
    });
    (port, state)
}

/// A span the layer exports arrives at the collector as a protobuf `POST
/// /v1/traces`, naming the service and carrying the turn's `gen_ai.*`
/// attributes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_span_reaches_the_collector() {
    let (port, collected) = collector().await;
    let endpoint = format!("http://127.0.0.1:{port}");
    let (layer, guard) = oxsum_server::telemetry::layer(&endpoint).expect("the layer builds");

    // Scoped, not global: the test binary's other suites must not inherit this
    // subscriber.
    let dispatch = tracing_subscriber::Registry::default().with(layer);
    tracing::subscriber::with_default(dispatch, || {
        let span = tracing::info_span!(
            "gen_ai.turn",
            gen_ai.operation.name = "chat",
            gen_ai.system = "openai",
            gen_ai.request.model = "m-test",
            gen_ai.usage.input_tokens = tracing::field::Empty,
            gen_ai.usage.output_tokens = tracing::field::Empty,
        );
        span.record("gen_ai.usage.input_tokens", 11_i64);
        span.record("gen_ai.usage.output_tokens", 7_i64);
        span.in_scope(|| {
            tracing::info!(oxsum.hold.freeze_minor = 42_i64, "hold taken");
        });
        span.in_scope(|| {
            tracing::info!(oxsum.charged_minor = 9_i64, "settle written");
        });
        drop(span);
    });

    // The batch exporter only ships on an interval or a flush: the guard's
    // shutdown forces it, the same way main flushes before exiting.
    guard.shutdown();

    let bodies = collected.bodies.lock().unwrap();
    assert_eq!(bodies.len(), 1, "the exporter POSTed once");
    let body = String::from_utf8_lossy(&bodies[0]);
    // The payload is protobuf, not JSON: assert on the field bytes that must be
    // present — the service name, the span name, and each `gen_ai.*` attribute
    // key the gateway writes.
    for needle in [
        "gen_ai.turn",
        "oxsum",
        "gen_ai.operation.name",
        "gen_ai.system",
        "gen_ai.request.model",
        "gen_ai.usage.input_tokens",
        "gen_ai.usage.output_tokens",
        "m-test",
        "chat",
        "hold taken",
        "settle written",
        "oxsum.hold.freeze_minor",
        "oxsum.charged_minor",
    ] {
        assert!(
            body.contains(needle),
            "the span payload is missing {needle:?}"
        );
    }
}
