//! OTLP trace export (issue #224): optional, env-gated on `OXSUM_OTLP_ENDPOINT`.
//!
//! The gateway already emits `tracing` spans around a turn; this module layers an
//! OpenTelemetry exporter onto the same subscriber so the spans ship to a collector
//! as OTLP/HTTP. Unset, nothing is installed and the feature costs nothing.
//!
//! Span attributes follow the GenAI semantic conventions (`gen_ai.*`): the
//! operation name is the surface's wire dialect, `gen_ai.system` the channel
//! protocol (`openai`/`anthropic`), `gen_ai.request.model` the model the caller
//! named, and the token counts are recorded once settlement knows them. As with
//! `/metrics` labels, attributes only ever carry bounded vocabularies — never a
//! request id, organization, or key.

use opentelemetry::trace::TracerProvider;
use opentelemetry_otlp::{SpanExporter, WithExportConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::runtime;
use opentelemetry_sdk::trace::{SdkTracerProvider, Tracer};
use tracing_subscriber::Registry;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// The shutdown handle: flushing the provider pushes the buffered spans to the
/// collector before the process exits. `Drop` alone cannot await the export, so
/// the binary calls [`OtelGuard::shutdown`] on the drain path.
pub struct OtelGuard {
    provider: SdkTracerProvider,
}

impl OtelGuard {
    /// Flushes every buffered span. Call before the process exits.
    pub fn shutdown(self) {
        // A failed flush is a log line, not a crash: the deployment may have run
        // for hours without the collector answering, and exiting is the job.
        if let Err(error) = self.provider.shutdown() {
            tracing::warn!(%error, "the OTLP exporter could not flush");
        }
    }
}

/// Builds the OTLP layer for `endpoint` (e.g. `http://localhost:4318`), under the
/// service name `oxsum`, and returns it with the guard the caller holds for the
/// process's life. The exporter is `http-proto` over reqwest — the workspace's
/// single TLS stack — so no gRPC codegen enters the tree.
///
/// The returned layer composes with `fmt` inside one `Registry`: the caller
/// installs them together, and every span both logs and exports under the same
/// env filter. The span fields the gateway sets under `gen_ai.*` arrive as OTLP
/// attributes unchanged.
pub fn layer<S>(
    endpoint: &str,
) -> Result<
    (
        tracing_opentelemetry::OpenTelemetryLayer<S, Tracer>,
        OtelGuard,
    ),
    Box<dyn std::error::Error>,
>
where
    S: tracing::Subscriber + for<'span> tracing_subscriber::registry::LookupSpan<'span>,
{
    let exporter = SpanExporter::builder()
        .with_http()
        .with_endpoint(format!("{endpoint}/v1/traces"))
        .build()?;
    // The default batch processor runs its own thread with no tokio reactor,
    // which the async reqwest client cannot export under. The async-runtime
    // variant takes `runtime::Tokio` so batches are exported on the caller's
    // runtime — this must be built inside `tokio::main`'s context.
    let batch =
        opentelemetry_sdk::trace::span_processor_with_async_runtime::BatchSpanProcessor::builder(
            exporter,
            runtime::Tokio,
        )
        .build();
    let provider = SdkTracerProvider::builder()
        .with_resource(Resource::builder().with_service_name("oxsum").build())
        .with_span_processor(batch)
        .build();
    let tracer: Tracer = provider.tracer("oxsum");
    Ok((
        tracing_opentelemetry::layer().with_tracer(tracer),
        OtelGuard { provider },
    ))
}

/// Installs fmt plus, when `OXSUM_OTLP_ENDPOINT` names a collector, the OTLP
/// layer: the one subscriber call the binary makes, in place of
/// `tracing_subscriber::fmt().init()`. The env filter applies to both outputs,
/// so `RUST_LOG` quiets traces the same way it quiets logs.
///
/// Returns the guard only when the layer was installed — an unset endpoint is
/// `None` and the binary runs exactly as before.
pub fn init(endpoint: Option<&str>) -> Result<Option<OtelGuard>, Box<dyn std::error::Error>> {
    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let fmt = tracing_subscriber::fmt::layer();
    match endpoint {
        Some(endpoint) => {
            let (otlp, guard) = layer(endpoint)?;
            Registry::default().with(filter).with(fmt).with(otlp).init();
            Ok(Some(guard))
        }
        None => {
            Registry::default().with(filter).with(fmt).init();
            Ok(None)
        }
    }
}
