// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! NAT traversal — UPnP / NAT-PMP port mapping (`specs/05` §6, runtime task #23
//! in `specs/17` §2). Byte-for-byte protocol parity is not a concern here (NAT
//! is a local LAN side-effect, invisible on the p2p wire); the port is a
//! faithful behavioural port of the Go `nat/` package:
//!
//! - [`NatRouter`] mirrors `nat.Router` — `supports_nat` / `map_port` /
//!   `unmap_port` / `external_ip`.
//! - [`get_router`] mirrors `nat.GetRouter`: probe UPnP first (via the
//!   `igd-next` IGD client), then NAT-PMP/PCP, else fall back to the no-op
//!   [`NoRouter`].
//! - [`port_mapper::PortMapper`] mirrors `nat.Mapper`: a background tokio task
//!   that (re)maps the staking port every `MAP_TIMEOUT = 30m` with up to
//!   `MAX_REFRESH_RETRIES = 3` attempts, and unmaps on shutdown driven by a
//!   [`tokio_util::sync::CancellationToken`].

use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::time::Duration;

use crate::{Error, Result};

pub mod port_mapper;

pub use port_mapper::{MAP_TIMEOUT, MAX_REFRESH_RETRIES, PortMapper};

/// Per-request timeout for the UPnP SOAP gateway search. Mirrors Go
/// `soapRequestTimeout = 10 * time.Second`. We map only the TCP staking port
/// (`PortMappingProtocol::TCP`, mirroring Go's uppercase `upnpProtocol = "TCP"`).
const SOAP_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// A public address used purely to discover our outbound interface IP when no
/// router is available (mirrors Go `googleDNSServer`). No packets are sent —
/// `connect` on a UDP socket only fixes the local routing decision.
const GOOGLE_DNS_SERVER: &str = "8.8.8.8:80";

/// Describes the functionality a network device must support to open ports to an
/// external IP. Port of `nat.Router`.
pub trait NatRouter: Send + Sync {
    /// True iff this router supports NAT traversal.
    fn supports_nat(&self) -> bool;

    /// Map external port `external` to internal port `internal` for `duration`,
    /// with a human-readable `desc`.
    fn map_port(&self, internal: u16, external: u16, desc: &str, duration: Duration) -> Result<()>;

    /// Undo a port mapping.
    fn unmap_port(&self, internal: u16, external: u16) -> Result<()>;

    /// Return our external IP.
    fn external_ip(&self) -> Result<IpAddr>;
}

/// Returns a router for the current network. Port of `nat.GetRouter`: probe UPnP
/// first, then NAT-PMP/PCP, else a no-op [`NoRouter`].
#[must_use]
pub fn get_router() -> Box<dyn NatRouter> {
    if let Some(r) = get_upnp_router() {
        return Box::new(r);
    }
    if let Some(r) = get_pmp_router() {
        return r;
    }
    Box::new(NoRouter::new())
}

/// A no-op router used when no UPnP/NAT-PMP gateway is found. Assumes the network
/// is already public. Port of `nat.noRouter`.
pub struct NoRouter {
    ip: Option<IpAddr>,
}

impl NoRouter {
    /// Returns a no-op router, recording the outbound interface IP (best-effort,
    /// mirrors `nat.NewNoRouter` → `getOutboundIP`).
    #[must_use]
    pub fn new() -> Self {
        Self {
            ip: get_outbound_ip(),
        }
    }
}

impl Default for NoRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl NatRouter for NoRouter {
    fn supports_nat(&self) -> bool {
        false
    }

    fn map_port(
        &self,
        _internal: u16,
        _external: u16,
        _desc: &str,
        _duration: Duration,
    ) -> Result<()> {
        // Mirrors Go errNoRouterCantMapPorts.
        Err(Error::NoRouter)
    }

    fn unmap_port(&self, _internal: u16, _external: u16) -> Result<()> {
        // Go's noRouter.UnmapPort is a no-op success.
        Ok(())
    }

    fn external_ip(&self) -> Result<IpAddr> {
        self.ip
            .ok_or_else(|| Error::Nat("getting outbound IP failed".to_string()))
    }
}

/// Best-effort discovery of our outbound interface IP. Port of
/// `nat.getOutboundIP`: open a UDP socket "connected" to a public address and
/// read back the kernel-chosen local address. No packets are actually sent.
fn get_outbound_ip() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect(GOOGLE_DNS_SERVER).ok()?;
    let local = socket.local_addr().ok()?;
    let addr = local.ip();
    // Unmap a 4-in-6 address to the bare IPv4 form (mirrors Go addr.Unmap()).
    let unmapped = match addr {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(addr, IpAddr::V4),
        IpAddr::V4(_) => addr,
    };
    Some(unmapped)
}

/// A UPnP IGD router backed by the `igd-next` synchronous client. Port of
/// `nat.upnpRouter`.
struct UpnpRouter {
    gateway: igd_next::Gateway,
    /// Our local LAN address on the interface facing the gateway, used as the
    /// `local_addr` of the port mapping (mirrors `upnpRouter.localIP`).
    local_addr: IpAddr,
}

impl NatRouter for UpnpRouter {
    fn supports_nat(&self) -> bool {
        true
    }

    fn map_port(&self, internal: u16, external: u16, desc: &str, duration: Duration) -> Result<()> {
        // go-nat / IGD use seconds; reject out-of-range lifetimes the way Go does
        // (errInvalidLifetime).
        let lifetime = u32::try_from(duration.as_secs())
            .map_err(|_| Error::Nat("invalid mapping duration range".to_string()))?;
        let local = SocketAddr::new(self.local_addr, internal);
        self.gateway
            .add_port(
                igd_next::PortMappingProtocol::TCP,
                external,
                local,
                lifetime,
                desc,
            )
            .map_err(|e| Error::Nat(e.to_string()))
    }

    fn unmap_port(&self, _internal: u16, external: u16) -> Result<()> {
        self.gateway
            .remove_port(igd_next::PortMappingProtocol::TCP, external)
            .map_err(|e| Error::Nat(e.to_string()))
    }

    fn external_ip(&self) -> Result<IpAddr> {
        self.gateway
            .get_external_ip()
            .map_err(|e| Error::Nat(e.to_string()))
    }
}

/// Searches for an internet gateway via UPnP IGD. Returns `None` if no gateway
/// is found (the CI / no-router case). Port of `nat.getUPnPRouter`.
fn get_upnp_router() -> Option<UpnpRouter> {
    let options = igd_next::SearchOptions {
        timeout: Some(SOAP_REQUEST_TIMEOUT),
        ..Default::default()
    };
    let gateway = igd_next::search_gateway(options).ok()?;
    // Determine the LAN address on the interface that reaches the gateway so the
    // mapping points back at this host (mirrors upnpRouter.localIP).
    let local_addr = local_ip_for_gateway(&gateway)?;
    Some(UpnpRouter {
        gateway,
        local_addr,
    })
}

/// Finds the local interface IP that routes to the gateway, mirroring
/// `upnpRouter.localIP`. Uses the same connected-UDP-socket trick as
/// [`get_outbound_ip`], targeting the gateway address.
fn local_ip_for_gateway(gateway: &igd_next::Gateway) -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect(gateway.addr).ok()?;
    socket.local_addr().ok().map(|a| a.ip())
}

/// Probes for a NAT-PMP / PCP gateway. Port of `nat.getPMPRouter`.
///
/// NAT-PMP support is not yet wired in the Rust port: `igd-next` is UPnP-only,
/// and the Go reference probes PMP only as a secondary fallback after UPnP. We
/// therefore always return `None` here so `get_router` falls through to the
/// no-op [`NoRouter`] when UPnP is unavailable — behaviourally identical to a
/// network with no PMP gateway. A real PMP probe (e.g. via `crab-nat`) is a
/// follow-up; called out in `tests/PORTING.md`.
fn get_pmp_router() -> Option<Box<dyn NatRouter>> {
    None
}
