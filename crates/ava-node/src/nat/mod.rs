// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! NAT port mapping (specs/12 §8, specs/17 §2 task #23).
//!
//! Mirrors avalanchego `nat/`: a `Router` trait with `upnp`, `pmp`, and
//! `no_router` implementations, the `Mapper` background task, and the
//! `dynamicip` updater that feeds the network's advertised IP.
//!
//! The `Router` trait ([`ava_network::nat::NatRouter`]), the UPnP router, the
//! no-op router, and the [`Mapper`] are reused from `ava-network` (which owns
//! the runtime networking — specs/05 §6) rather than re-implemented here. This
//! crate adds the two pieces `ava-network` left as follow-ups:
//!
//! - [`pmp`] — a hand-rolled RFC 6886 NAT-PMP router (the `ava-network`
//!   `get_pmp_router` probe is a `None` stub).
//! - [`dynamicip`] — the public-IP resolution updater (runtime task #23).
//!
//! [`get_router`] probes UPnP (via `ava-network`) first, then NAT-PMP, then the
//! no-op router — mirroring Go `nat.GetRouter`'s ordering now that PMP exists.

pub mod dynamicip;
pub mod pmp;

// Re-export the `ava-network` NAT surface so node assembly names a single
// `nat` module (the `upnp`/`noop` impls + the `Mapper` live there).
pub use ava_network::nat::{
    MAP_TIMEOUT, MAX_REFRESH_RETRIES, NatRouter, NoRouter, PortMapper as Mapper,
    get_router as get_upnp_or_noop_router,
};

pub use self::pmp::PmpRouter;

/// Returns a router for the current network, mirroring Go `nat.GetRouter`:
/// probe UPnP first (via `ava-network`'s `igd-next` IGD client), then NAT-PMP,
/// else fall back to the no-op [`NoRouter`].
///
/// Unlike [`ava_network::nat::get_router`] (which lacks a PMP probe and so
/// falls straight to the no-op router when UPnP is unavailable), this includes
/// the [`pmp`] fallback, restoring full parity with the Go ordering.
#[must_use]
pub fn get_router() -> Box<dyn NatRouter> {
    let upnp_or_noop = get_upnp_or_noop_router();
    // `ava-network::get_router` returns a `NoRouter` only when UPnP probing
    // failed; in that case try NAT-PMP before accepting the no-op.
    if upnp_or_noop.supports_nat() {
        return upnp_or_noop;
    }
    if let Some(pmp) = pmp::get_pmp_router() {
        return Box::new(pmp);
    }
    upnp_or_noop
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn noop_router_maps_nothing() {
        let router = NoRouter::new();
        assert!(!router.supports_nat());
        // The no-op router refuses to map ports (mirrors errNoRouterCantMapPorts)
        // but unmapping is a no-op success.
        assert!(
            router
                .map_port(9651, 9651, "avalanche", Duration::from_secs(60))
                .is_err()
        );
        assert!(router.unmap_port(9651, 9651).is_ok());
    }
}
