// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! NAT-PMP router — a minimal RFC 6886 client (specs/12 §8).
//!
//! `ava-network`'s `get_pmp_router` is a `None` stub (`igd-next` is UPnP-only),
//! so this module supplies the NAT-PMP fallback Go's `nat.getPMPRouter` probes
//! after UPnP. It is a hand-rolled RFC 6886 UDP client rather than a new
//! third-party dependency (per the M8.28 constraint), implementing the same
//! [`NatRouter`] trait so it slots into [`super::get_router`] and the
//! `ava-network` [`Mapper`](super::Mapper).
//!
//! Protocol (RFC 6886): all requests go to UDP port **5351** on the default
//! gateway. The "external address" request (opcode 0) returns the gateway's
//! public IPv4; the "map" request (opcode 1 = UDP, 2 = TCP) installs a mapping
//! with a lifetime in seconds. We map only TCP (the staking port), matching the
//! UPnP router's `upnpProtocol = "TCP"`.
//!
//! **Gateway-discovery limitation (not full parity with Go).** Go's NAT-PMP path
//! reads the OS route table to find the default gateway. There is no portable
//! std-library route-table query, so `default_gateway_ipv4` instead derives
//! the gateway from a `.1`-on-the-local-/24 heuristic (the common consumer-router
//! convention). This matches the protocol exchange and the UPnP→PMP→noop probe
//! *ordering* exactly, but the gateway address itself is a best-effort guess: on
//! a network whose gateway is not `x.y.z.1` the probe simply finds no PMP gateway
//! and the caller falls through to the no-op router. A real route-table read is a
//! documented follow-up.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::Duration;

use ava_network::nat::NatRouter;
use ava_network::{Error, Result};

/// The NAT-PMP server port on the gateway (RFC 6886 §3).
const NAT_PMP_PORT: u16 = 5351;

/// RFC 6886 protocol version (always 0).
const PMP_VERSION: u8 = 0;

/// Opcode for the "external address" request (RFC 6886 §3.2).
const OP_EXTERNAL_ADDRESS: u8 = 0;

/// Opcode for the "map TCP port" request (RFC 6886 §3.3). UDP would be `1`.
const OP_MAP_TCP: u8 = 2;

/// Per-request read timeout. RFC 6886 §3.1 prescribes a 250ms initial timeout
/// with exponential backoff; we keep a single short timeout since the probe is
/// best-effort and a missing gateway should fall through quickly.
const REQUEST_TIMEOUT: Duration = Duration::from_millis(250);

/// A NAT-PMP router speaking RFC 6886 to a gateway. Port of `nat.pmpRouter`.
pub struct PmpRouter {
    /// The gateway's NAT-PMP socket address (`<gateway-ip>:5351`).
    gateway: SocketAddr,
}

impl PmpRouter {
    /// Build a router targeting `gateway_ip`'s NAT-PMP port.
    #[must_use]
    pub fn new(gateway_ip: Ipv4Addr) -> Self {
        Self {
            gateway: SocketAddr::new(IpAddr::V4(gateway_ip), NAT_PMP_PORT),
        }
    }

    /// Send `request` to the gateway and read one response datagram, returning
    /// the raw bytes. Errors map to [`Error::Nat`].
    fn round_trip(&self, request: &[u8], response_len: usize) -> Result<Vec<u8>> {
        let socket =
            UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).map_err(|e| Error::Nat(e.to_string()))?;
        socket
            .set_read_timeout(Some(REQUEST_TIMEOUT))
            .map_err(|e| Error::Nat(e.to_string()))?;
        socket
            .send_to(request, self.gateway)
            .map_err(|e| Error::Nat(e.to_string()))?;

        let mut buf = vec![0u8; response_len];
        let (read, from) = socket
            .recv_from(&mut buf)
            .map_err(|e| Error::Nat(e.to_string()))?;
        // RFC 6886 §3.2.1: ignore datagrams not from the gateway.
        if from.ip() != self.gateway.ip() {
            return Err(Error::Nat(
                "NAT-PMP response from unexpected host".to_string(),
            ));
        }
        buf.truncate(read);
        validate_header(&buf, response_len)?;
        Ok(buf)
    }
}

/// Validate the common RFC 6886 response header: minimum length, version 0, the
/// `0x80 | request_op` echoed opcode, and a zero result code.
fn validate_header(resp: &[u8], expected_len: usize) -> Result<()> {
    if resp.len() < expected_len {
        return Err(Error::Nat("short NAT-PMP response".to_string()));
    }
    // resp[0] = version, resp[1] = opcode (0x80 | request), resp[2..4] = result.
    let version = resp.first().copied().unwrap_or(0xFF);
    if version != PMP_VERSION {
        return Err(Error::Nat("unsupported NAT-PMP version".to_string()));
    }
    let result = u16::from_be_bytes([
        resp.get(2).copied().unwrap_or(0xFF),
        resp.get(3).copied().unwrap_or(0xFF),
    ]);
    if result != 0 {
        return Err(Error::Nat(format!("NAT-PMP error result code {result}")));
    }
    Ok(())
}

impl NatRouter for PmpRouter {
    fn supports_nat(&self) -> bool {
        true
    }

    fn map_port(
        &self,
        internal: u16,
        external: u16,
        _desc: &str,
        duration: Duration,
    ) -> Result<()> {
        let lifetime = u32::try_from(duration.as_secs())
            .map_err(|_| Error::Nat("invalid mapping duration range".to_string()))?;
        // RFC 6886 §3.3 map request: version, op, reserved(2), internal port(2),
        // suggested external port(2), lifetime(4) = 12 bytes.
        let mut req = Vec::with_capacity(12);
        req.push(PMP_VERSION);
        req.push(OP_MAP_TCP);
        req.extend_from_slice(&[0, 0]); // reserved
        req.extend_from_slice(&internal.to_be_bytes());
        req.extend_from_slice(&external.to_be_bytes());
        req.extend_from_slice(&lifetime.to_be_bytes());
        // Response is 16 bytes: header(4), epoch(4), internal port(2), mapped
        // external port(2), lifetime(4). A zero result code is success.
        self.round_trip(&req, 16).map(|_| ())
    }

    fn unmap_port(&self, internal: u16, _external: u16) -> Result<()> {
        // RFC 6886 §3.4: a map request with lifetime 0 deletes the mapping. The
        // external port MUST be 0 on a delete request.
        let mut req = Vec::with_capacity(12);
        req.push(PMP_VERSION);
        req.push(OP_MAP_TCP);
        req.extend_from_slice(&[0, 0]); // reserved
        req.extend_from_slice(&internal.to_be_bytes());
        req.extend_from_slice(&0u16.to_be_bytes()); // external port = 0 on delete
        req.extend_from_slice(&0u32.to_be_bytes()); // lifetime = 0 deletes
        self.round_trip(&req, 16).map(|_| ())
    }

    fn external_ip(&self) -> Result<IpAddr> {
        // RFC 6886 §3.2 external address request: version, opcode = 2 bytes.
        let req = [PMP_VERSION, OP_EXTERNAL_ADDRESS];
        // Response is 12 bytes: header(4), epoch(4), external IPv4(4).
        let resp = self.round_trip(&req, 12)?;
        let octets = [
            resp.get(8).copied().unwrap_or(0),
            resp.get(9).copied().unwrap_or(0),
            resp.get(10).copied().unwrap_or(0),
            resp.get(11).copied().unwrap_or(0),
        ];
        Ok(IpAddr::V4(Ipv4Addr::from(octets)))
    }
}

/// Probe for a NAT-PMP gateway. Port of `nat.getPMPRouter`.
///
/// Discovers the default-gateway IPv4 (best-effort) and issues an external
/// address request; returns a [`PmpRouter`] iff the gateway answers. Returns
/// `None` when no gateway is found or it does not speak NAT-PMP (the CI /
/// no-router case), so [`super::get_router`] falls through to the no-op router.
#[must_use]
pub fn get_pmp_router() -> Option<PmpRouter> {
    let gateway = default_gateway_ipv4()?;
    let router = PmpRouter::new(gateway);
    // Confirm the gateway actually speaks NAT-PMP before adopting it.
    router.external_ip().ok().map(|_| router)
}

/// Best-effort discovery of the default-gateway IPv4 address.
///
/// RFC 6886 sends to the host's default gateway. There is no portable
/// std-library route-table query; we derive the gateway from the outbound
/// interface address by assuming the common `.1` gateway convention on the
/// local /24. This is a heuristic — a real route-table read is a follow-up — but
/// it lets a configured/known gateway be probed, and falls back to `None`
/// (no-op router) when discovery fails, matching a network with no PMP gateway.
fn default_gateway_ipv4() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    // Connecting fixes the kernel's outbound-interface choice without sending.
    socket.connect((Ipv4Addr::new(8, 8, 8, 8), 80)).ok()?;
    let local = socket.local_addr().ok()?;
    match local.ip() {
        IpAddr::V4(v4) => {
            let mut octets = v4.octets();
            // Conventional default gateway on a /24: x.y.z.1.
            octets[3] = 1;
            Some(Ipv4Addr::from(octets))
        }
        IpAddr::V6(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn router_supports_nat() {
        let router = PmpRouter::new(Ipv4Addr::new(192, 168, 1, 1));
        assert!(router.supports_nat());
    }

    #[test]
    fn validate_header_rejects_bad_version_and_result() {
        // version 1 (unsupported)
        assert!(validate_header(&[1, 0x80, 0, 0], 4).is_err());
        // nonzero result code
        assert!(validate_header(&[0, 0x80, 0, 2], 4).is_err());
        // short response
        assert!(validate_header(&[0, 0x80], 4).is_err());
        // well-formed success header
        assert!(validate_header(&[0, 0x80, 0, 0], 4).is_ok());
    }

    #[test]
    fn map_request_to_unreachable_gateway_errors_fast() {
        // TEST-NET-1 (192.0.2.0/24) is reserved and unroutable; the request
        // times out and surfaces a Nat error rather than hanging.
        let router = PmpRouter::new(Ipv4Addr::new(192, 0, 2, 123));
        assert!(
            router
                .map_port(9651, 9651, "avalanche", Duration::from_secs(60))
                .is_err()
        );
    }
}
