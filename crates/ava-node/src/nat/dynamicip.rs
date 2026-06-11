// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Dynamic public-IP updater (specs/12 §8, specs/17 §2 task #23).
//!
//! Mirrors avalanchego `utils/dynamicip`: a background task that resolves the
//! node's public IP on `--public-ip-resolution-frequency` and pushes any change
//! into a sink (the network's advertised IP — `05`). Go offers `opendns`,
//! `ifconfigco`, and `ifconfigme` resolvers plus a static no-op resolver.
//!
//! The resolver is abstracted behind the [`Resolver`] trait so the updater is
//! unit-testable without network access; concrete HTTP/DNS resolvers are a
//! follow-up wired by `Node::new` (M8.29). The IP sink is a caller-supplied
//! callback so this module does not couple to the network's internal IP state.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// The public-IP resolution services Go supports (`--public-ip-resolution-service`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolverService {
    /// `opendns` (DNS query against OpenDNS resolvers).
    OpenDns,
    /// `ifconfigco` (HTTPS to ifconfig.co).
    IfConfigCo,
    /// `ifconfigme` (HTTPS to ifconfig.me).
    IfConfigMe,
}

/// Resolves the node's current public IP. Port of `dynamicip.Resolver`.
#[async_trait]
pub trait Resolver: Send + Sync {
    /// Resolve the current public IP.
    ///
    /// # Errors
    /// Returns an error string when resolution fails (DNS/HTTP error, parse
    /// failure, …); the updater logs and retries on the next tick.
    async fn resolve(&self) -> Result<IpAddr, String>;
}

/// A sink for resolved IP changes — typically the network's advertised-IP
/// setter (`05`). Invoked only when the IP actually changes.
pub type IpSink = Arc<dyn Fn(IpAddr) + Send + Sync>;

/// The dynamic-IP updater. Port of `dynamicip.Updater`.
pub struct Updater {
    resolver: Arc<dyn Resolver>,
    sink: IpSink,
    frequency: Duration,
}

impl Updater {
    /// Build an updater that re-resolves every `frequency` and pushes changes
    /// into `sink`.
    #[must_use]
    pub fn new(resolver: Arc<dyn Resolver>, sink: IpSink, frequency: Duration) -> Self {
        Self {
            resolver,
            sink,
            frequency,
        }
    }

    /// Spawn the background resolution loop. It ticks every `frequency`,
    /// resolving the public IP and invoking the sink on change, until `token`
    /// is cancelled (node shutdown step 10). Port of `dynamicip.Updater.Dispatch`.
    #[must_use]
    pub fn start(self, token: CancellationToken) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut last: Option<IpAddr> = None;
            let mut interval = tokio::time::interval(self.frequency);
            loop {
                tokio::select! {
                    () = token.cancelled() => return,
                    _ = interval.tick() => {
                        if let Ok(ip) = self.resolver.resolve().await
                            && last != Some(ip)
                        {
                            (self.sink)(ip);
                            last = Some(ip);
                        }
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct FixedResolver(IpAddr);

    #[async_trait]
    impl Resolver for FixedResolver {
        async fn resolve(&self) -> Result<IpAddr, String> {
            Ok(self.0)
        }
    }

    #[tokio::test]
    async fn pushes_resolved_ip_once_then_stops_on_cancel() {
        let ip = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7));
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(parking_lot::Mutex::new(None));
        let calls_c = calls.clone();
        let seen_c = seen.clone();
        let sink: IpSink = Arc::new(move |got| {
            calls_c.fetch_add(1, Ordering::SeqCst);
            *seen_c.lock() = Some(got);
        });

        let updater = Updater::new(Arc::new(FixedResolver(ip)), sink, Duration::from_millis(5));
        let token = CancellationToken::new();
        let handle = updater.start(token.clone());

        // Let a few ticks elapse, then cancel and join.
        tokio::time::sleep(Duration::from_millis(40)).await;
        token.cancel();
        handle.await.expect("updater task joins");

        // The IP never changes, so the sink fires exactly once.
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(*seen.lock(), Some(ip));
    }
}
