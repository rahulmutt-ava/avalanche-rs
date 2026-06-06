// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The [`PortMapper`] background task — port of `nat.Mapper`.
//!
//! It maps the staking port on start (retrying up to [`MAX_REFRESH_RETRIES`]
//! times with a 1s delay), then re-maps every [`MAP_TIMEOUT`] until its
//! [`CancellationToken`] fires, at which point it unmaps the port and joins.
//! Runtime task #23 in `specs/17` §2.

use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::nat::NatRouter;

/// How often the background task re-maps the port. Go: `mapTimeout = 30m`.
pub const MAP_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// How many times a single map attempt is retried (with a 1s delay) before
/// giving up. Go: `maxRefreshRetries = 3`.
pub const MAX_REFRESH_RETRIES: u32 = 3;

/// Delay between map retries. Go: `time.Sleep(1 * time.Second)`.
const RETRY_DELAY: Duration = Duration::from_secs(1);

/// Maps a port on a [`NatRouter`] and keeps the mapping alive in the background.
/// Port of `nat.Mapper`.
pub struct PortMapper<R: NatRouter + 'static> {
    router: Arc<R>,
    /// Re-map cadence. Defaults to [`MAP_TIMEOUT`]; overridable for tests.
    update_time: Duration,
}

impl<R: NatRouter + 'static> PortMapper<R> {
    /// Returns a new mapper over `router`, re-mapping every [`MAP_TIMEOUT`].
    #[must_use]
    pub fn new(router: Arc<R>) -> Self {
        Self {
            router,
            update_time: MAP_TIMEOUT,
        }
    }

    /// Overrides the re-map cadence (the lease lifetime stays [`MAP_TIMEOUT`],
    /// matching Go, where `retryMapPort` always passes `mapTimeout`). Intended
    /// for deterministic tests.
    #[must_use]
    pub fn with_update_time(mut self, update_time: Duration) -> Self {
        self.update_time = update_time;
        self
    }

    /// Maps `external` → `internal` (description `desc`) and spawns the
    /// background keep-alive task. The returned [`JoinHandle`] resolves once the
    /// task has unmapped the port and exited, after `token` is cancelled. Port
    /// of `nat.Mapper.Map`.
    ///
    /// If the router does not support NAT, this is a no-op: the returned handle
    /// resolves immediately.
    pub fn start(
        &self,
        internal: u16,
        external: u16,
        desc: &str,
        token: CancellationToken,
    ) -> JoinHandle<()> {
        let router = self.router.clone();
        let update_time = self.update_time;
        let desc = desc.to_string();

        tokio::spawn(async move {
            if !router.supports_nat() {
                return;
            }

            // Initial map attempt (best-effort; failures are logged in Go and
            // simply tolerated here — the keep-alive loop will retry).
            let _ = retry_map_port(router.as_ref(), internal, external, &desc).await;

            keep_port_mapping(router, internal, external, desc, update_time, token).await;
        })
    }
}

/// Retries `map_port` up to [`MAX_REFRESH_RETRIES`] times with a 1s delay. Port
/// of `nat.Mapper.retryMapPort`.
async fn retry_map_port<R: NatRouter + ?Sized>(
    router: &R,
    internal: u16,
    external: u16,
    desc: &str,
) -> crate::Result<()> {
    let mut last_err = None;
    for _ in 0..MAX_REFRESH_RETRIES {
        match router.map_port(internal, external, desc, MAP_TIMEOUT) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = Some(e);
                tokio::time::sleep(RETRY_DELAY).await;
            }
        }
    }
    Err(last_err.unwrap_or(crate::Error::NoRouter))
}

/// Keeps the port mapped: re-maps every `update_time` until `token` is
/// cancelled, then unmaps on the way out. Port of `nat.Mapper.keepPortMapping`
/// (the `defer` unmap + the `select` on the update timer vs. the closer).
async fn keep_port_mapping<R: NatRouter + 'static>(
    router: Arc<R>,
    internal: u16,
    external: u16,
    desc: String,
    update_time: Duration,
    token: CancellationToken,
) {
    let mut timer = tokio::time::interval(update_time);
    // The first `tick()` completes immediately; consume it so the loop fires on
    // the cadence rather than re-mapping again right away.
    timer.tick().await;

    loop {
        tokio::select! {
            biased;
            () = token.cancelled() => {
                break;
            }
            _ = timer.tick() => {
                let _ = retry_map_port(router.as_ref(), internal, external, &desc).await;
            }
        }
    }

    // Unmap on shutdown (Go's deferred UnmapPort). Errors are tolerated — the
    // lease will expire on its own.
    let _ = router.unmap_port(internal, external);
}
