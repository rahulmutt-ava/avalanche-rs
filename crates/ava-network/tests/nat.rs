// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Integration tests for the NAT port mapper. Ports `nat/nat.go` +
//! `nat/no_router.go` (the `Router`/`Mapper` surface). The UPnP/NAT-PMP probe
//! (`nat/upnp.go`, `nat/pmp.go`) is exercised only through `get_router()`'s
//! no-gateway fallback — CI has no UPnP/PMP gateway, so it must return the
//! no-op router. The `PortMapper` background task is driven over a recording
//! mock `NatRouter` with `tokio::time` paused so the refresh interval is
//! deterministic (no wall-clock sleeps, per `specs/02` testing strategy).

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing
)]

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

use ava_network::nat::{NatRouter, PortMapper, get_router};
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

/// A recording mock router: counts `map_port`/`unmap_port` calls so the test
/// can assert the `PortMapper` mapped on start and unmapped on shutdown.
#[derive(Default)]
struct MockRouter {
    map_calls: Mutex<Vec<(u16, u16)>>,
    unmap_calls: Mutex<Vec<(u16, u16)>>,
}

impl NatRouter for MockRouter {
    fn supports_nat(&self) -> bool {
        true
    }

    fn map_port(
        &self,
        internal: u16,
        external: u16,
        _desc: &str,
        _duration: Duration,
    ) -> ava_network::Result<()> {
        self.map_calls.lock().push((internal, external));
        Ok(())
    }

    fn unmap_port(&self, internal: u16, external: u16) -> ava_network::Result<()> {
        self.unmap_calls.lock().push((internal, external));
        Ok(())
    }

    fn external_ip(&self) -> ava_network::Result<IpAddr> {
        Ok(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7)))
    }
}

#[test]
fn get_router_falls_back_to_no_router() {
    // In CI there is no UPnP/NAT-PMP gateway, so `get_router()` must return the
    // no-op `NoRouter`, whose `supports_nat()` is `false`. The probe must never
    // panic and the result must be self-consistent: a no-op router refuses to
    // map ports.
    let router = get_router();
    assert!(
        !router.supports_nat(),
        "with no gateway, get_router() must return the no-op router"
    );
    // The no-op router refuses to map ports (mirrors Go errNoRouterCantMapPorts).
    assert!(
        router
            .map_port(9651, 9651, "avalanche", Duration::from_secs(60))
            .is_err(),
        "no-op router must refuse to map a port"
    );
    // Unmapping is a no-op success on the no-op router.
    assert!(router.unmap_port(9651, 9651).is_ok());
}

#[tokio::test(start_paused = true)]
async fn port_mapper_unmaps_on_shutdown() {
    let router = Arc::new(MockRouter::default());
    let token = CancellationToken::new();

    let mapper = PortMapper::new(router.clone());
    let internal_port = 9651u16;
    let external_port = 9651u16;

    // Start the background task; it maps immediately, then re-maps every
    // `map_timeout` until cancelled.
    let handle = mapper.start(internal_port, external_port, "avalanche", token.clone());

    // Let the initial map happen.
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::task::yield_now().await;

    assert_eq!(
        router.map_calls.lock().as_slice(),
        &[(internal_port, external_port)],
        "PortMapper must map the staking port once on start"
    );

    // Shutdown via the cancellation token; the task must unmap on its way out.
    token.cancel();
    handle.await.expect("PortMapper task must join cleanly");

    assert_eq!(
        router.unmap_calls.lock().as_slice(),
        &[(internal_port, external_port)],
        "PortMapper must unmap the staking port on shutdown"
    );
}
