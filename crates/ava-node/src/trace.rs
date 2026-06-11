// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! OpenTelemetry trace wiring (specs/12 §7, 18 §6).
//!
//! Mirrors avalanchego `trace/`. [`new`] turns a resolved
//! [`TraceConfig`] into a [`Tracer`]:
//!
//! - `--tracing-exporter-type=disabled` ⇒ a no-op tracer (mirror `noop.go`):
//!   no exporter, no subscriber layer, nothing to flush.
//! - `grpc` / `http` ⇒ an `opentelemetry-otlp` `SpanExporter` (OTLP over tonic
//!   gRPC or HTTP) feeding an `opentelemetry_sdk` `TracerProvider` sampled by
//!   `Sampler::TraceIdRatioBased(rate)`, wrapped by `tracing-opentelemetry`'s
//!   `OpenTelemetryLayer`. `--tracing-insecure` controls TLS, `--tracing-headers`
//!   become OTLP metadata, and the resource carries the service name/version so
//!   spans group identically to the Go node in the backend.
//!
//! The caller adds [`Tracer::layer`] to the §5.4 subscriber when present and
//! calls [`Tracer::shutdown`] on node shutdown (step 14) to flush spans.

use std::time::Duration;

use ava_config::node::{TraceConfig, TraceExporterType};
use opentelemetry::KeyValue;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::{SpanExporter, WithExportConfig, WithTonicConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::runtime;
use opentelemetry_sdk::trace::{Sampler, TracerProvider};
use tonic::metadata::{MetadataKey, MetadataMap, MetadataValue};
use tonic::transport::ClientTlsConfig;
use tracing::Subscriber;
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::registry::LookupSpan;

/// Default per-export timeout for the OTLP exporter (mirrors Go's
/// `exporterClientTimeout` of 10s used by the collector client).
const EXPORT_TIMEOUT: Duration = Duration::from_secs(10);

/// Errors raised while building the OTLP exporter / tracer provider.
#[derive(Debug, thiserror::Error)]
pub enum TraceError {
    /// The OTLP exporter could not be constructed (bad endpoint, transport, …).
    #[error("building OTLP span exporter: {0}")]
    Exporter(String),
    /// A `--tracing-headers` entry was not valid for OTLP metadata.
    #[error("invalid tracing header {key:?}: {reason}")]
    Header {
        /// The offending header key.
        key: String,
        /// The underlying parse failure.
        reason: String,
    },
}

/// Convenience result alias for this module.
pub type Result<T> = std::result::Result<T, TraceError>;

/// A node tracer: either an OTLP-backed provider + its subscriber layer, or a
/// no-op when tracing is disabled (mirror `trace.noOpTracer`).
///
/// The subscriber the [`layer`](Tracer::layer) attaches to is chosen by the
/// caller at the `layer`/`with_layer` call site (the type parameter `S`).
pub struct Tracer {
    provider: Option<TracerProvider>,
}

impl Tracer {
    /// Whether tracing is enabled (an OTLP provider was installed).
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.provider.is_some()
    }

    /// The `tracing-opentelemetry` layer to add to the §5.4 subscriber, or
    /// `None` when tracing is disabled (the layer is *not added*, matching Go's
    /// no-op tracer — see specs/18 §6).
    #[must_use]
    pub fn layer<S>(&self) -> Option<OpenTelemetryLayer<S, opentelemetry_sdk::trace::Tracer>>
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        let provider = self.provider.as_ref()?;
        let tracer = provider.tracer(SERVICE_NAME);
        Some(tracing_opentelemetry::layer().with_tracer(tracer))
    }

    /// Add this tracer's layer to `subscriber` when enabled, returning the
    /// subscriber unchanged when disabled. A convenience for the §5.4 wiring.
    #[must_use]
    pub fn with_layer<S>(&self, subscriber: S) -> Box<dyn Subscriber + Send + Sync>
    where
        S: Subscriber + Send + Sync + for<'a> LookupSpan<'a>,
    {
        use tracing_subscriber::layer::SubscriberExt;
        match self.layer::<S>() {
            Some(layer) => Box::new(subscriber.with(layer)),
            None => Box::new(subscriber),
        }
    }

    /// Flush and shut down the tracer provider (node shutdown step 14). A no-op
    /// when tracing is disabled.
    ///
    /// # Errors
    /// Returns [`TraceError::Exporter`] if the provider fails to flush/shutdown.
    pub fn shutdown(&self) -> Result<()> {
        if let Some(provider) = self.provider.as_ref() {
            provider
                .shutdown()
                .map_err(|e| TraceError::Exporter(e.to_string()))?;
        }
        Ok(())
    }
}

/// The OTLP resource service name (mirror Go `constants.AppName`).
const SERVICE_NAME: &str = "avalanchego";

/// Build a [`Tracer`] from the resolved [`TraceConfig`] (specs/12 §7).
///
/// Returns a no-op tracer when `cfg.exporter_type` is
/// [`TraceExporterType::Disabled`]; otherwise builds an OTLP exporter over the
/// selected transport.
///
/// # Errors
/// - [`TraceError::Header`] if a `--tracing-headers` entry is not valid OTLP
///   metadata.
/// - [`TraceError::Exporter`] if the OTLP exporter cannot be built.
pub fn new(cfg: &TraceConfig) -> Result<Tracer> {
    match cfg.exporter_type {
        TraceExporterType::Disabled => Ok(Tracer { provider: None }),
        TraceExporterType::Grpc => build_provider(cfg, build_grpc_exporter(cfg)?),
        TraceExporterType::Http => build_provider(cfg, build_http_exporter(cfg)?),
    }
}

/// Assemble the `TracerProvider` around an already-built exporter, applying the
/// ratio sampler and resource attributes.
fn build_provider(cfg: &TraceConfig, exporter: SpanExporter) -> Result<Tracer> {
    let resource = Resource::new([
        KeyValue::new("service.name", SERVICE_NAME),
        KeyValue::new("service.version", cfg.version.clone()),
    ]);

    let provider = TracerProvider::builder()
        .with_batch_exporter(exporter, runtime::Tokio)
        .with_sampler(Sampler::TraceIdRatioBased(cfg.trace_sample_rate))
        .with_resource(resource)
        .build();

    Ok(Tracer {
        provider: Some(provider),
    })
}

/// Build the OTLP gRPC (tonic) exporter, applying endpoint, timeout, TLS and
/// header metadata.
fn build_grpc_exporter(cfg: &TraceConfig) -> Result<SpanExporter> {
    let mut metadata = MetadataMap::new();
    for (key, value) in &cfg.headers {
        let name = MetadataKey::from_bytes(key.as_bytes()).map_err(|e| TraceError::Header {
            key: key.clone(),
            reason: e.to_string(),
        })?;
        let val = value
            .parse::<MetadataValue<_>>()
            .map_err(|e| TraceError::Header {
                key: key.clone(),
                reason: e.to_string(),
            })?;
        metadata.insert(name, val);
    }

    let mut builder = SpanExporter::builder()
        .with_tonic()
        .with_endpoint(cfg.endpoint.clone())
        .with_timeout(EXPORT_TIMEOUT)
        .with_metadata(metadata);

    // `--tracing-insecure` (default true): no TLS to the collector. When TLS is
    // requested, attach a client TLS config (system trust roots) so tonic
    // upgrades the channel; otherwise leave it plaintext.
    if !cfg.insecure {
        builder = builder.with_tls_config(ClientTlsConfig::new().with_native_roots());
    }

    builder
        .build()
        .map_err(|e| TraceError::Exporter(e.to_string()))
}

/// Build the OTLP HTTP exporter, applying endpoint and timeout.
fn build_http_exporter(cfg: &TraceConfig) -> Result<SpanExporter> {
    SpanExporter::builder()
        .with_http()
        .with_endpoint(cfg.endpoint.clone())
        .with_timeout(EXPORT_TIMEOUT)
        .build()
        .map_err(|e| TraceError::Exporter(e.to_string()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn disabled_config() -> TraceConfig {
        TraceConfig {
            exporter_type: TraceExporterType::Disabled,
            endpoint: String::new(),
            insecure: true,
            headers: HashMap::new(),
            trace_sample_rate: 0.1,
            app_name: "avalanchego".to_owned(),
            version: "1.0.0".to_owned(),
        }
    }

    #[test]
    fn disabled_is_noop() {
        // `--tracing-exporter-type=disabled` ⇒ no OTel layer (specs/12 §7).
        let tracer = new(&disabled_config()).expect("disabled tracer never errors");
        assert!(!tracer.is_enabled());
        assert!(
            tracer.layer::<tracing_subscriber::Registry>().is_none(),
            "disabled tracer must add no subscriber layer"
        );
        // Shutdown is a no-op for the disabled tracer.
        tracer.shutdown().expect("disabled shutdown is infallible");
    }
}
