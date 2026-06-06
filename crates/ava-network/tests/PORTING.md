# ava-network — porting matrix (Wave B: M2.7–M2.10)

Tracks the Go `network/peer` TLS + identity surfaces ported in M2 Wave B. The
peer actor / runtime / message queue / throttlers (M2.11+) are a later wave and
are NOT covered here.

| Go source | Rust home | Test(s) | Status |
|---|---|---|---|
| `network/peer/tls_config.go::TLSConfig` (TLS1.3-only, mutual, no SNI/ALPN) | `peer::tls_config::{server_config, client_config}` | `tls_config::configs_are_tls13_only_and_mutual` | done |
| `network/peer/tls_config.go::ValidateCertificate` (leaf-key policy) | `peer::verifier::validate_leaf_public_key` + `peer::verifier::danger::{AvaClientCertVerifier, AvaServerCertVerifier}` | `verifier::accepts_p256_rejects_others` | done |
| `network/peer/upgrader.go::Upgrade` / `connToIDAndCert` | `peer::upgrader::Upgrader::upgrade` | `tls_handshake::{loopback_mutual_tls_derives_node_id, rejects_non_p256}` | done |
| `ids/node_id.go::NodeIDFromCert` (RIPEMD160(SHA256(DER))) | `peer::upgrader::node_id_from_cert{,_der}` (→ `ava_crypto::staking::node_id_from_cert`) | `golden_tls::node_id_from_cert_golden` (vector `tests/vectors/tls/staker.json`) | done |
| `network/peer/ip.go::UnsignedIP.bytes` (`As16()(16) ‖ port_be ‖ ts_be`) | `peer::ip::UnsignedIp::bytes` | `signed_ip::unsigned_ip_bytes_layout` (vector `tests/vectors/message/signed_ip.json`) | done |
| `network/peer/ip.go::UnsignedIP.Sign` (TLS sig over SHA256(bytes) + BLS PoP over raw bytes) | `peer::ip::UnsignedIp::sign` | `signed_ip::{signed_ip_verify_roundtrip, bls_proof_of_possession_over_raw_bytes}` | done |
| `network/peer/ip.go::SignedIP.Verify` (ts ≤ max, valid TLS sig) | `peer::ip::SignedIp::verify` | `signed_ip::signed_ip_verify_roundtrip` | done |
| `network/peer/ip_signer.go::IPSigner` (cached signed IP, re-sign on IP change) | `peer::ip_signer::IpSigner` (`arc_swap::ArcSwapOption`) | covered indirectly (sign path); dedicated cache test deferred | done |

## NAT (Wave C: M2.19)

Ports the Go `nat/` package (UPnP / NAT-PMP port mapping; `specs/05` §6,
runtime task #23 in `specs/17` §2).

| Go source | Rust home | Test(s) | Status |
|---|---|---|---|
| `nat/nat.go::Router` (interface) | `nat::NatRouter` | `nat::{get_router_falls_back_to_no_router, port_mapper_unmaps_on_shutdown}` | done |
| `nat/nat.go::GetRouter` (probe UPnP → PMP → NoRouter) | `nat::get_router` | `nat::get_router_falls_back_to_no_router` | done |
| `nat/no_router.go::noRouter` + `getOutboundIP` | `nat::NoRouter` + `nat::get_outbound_ip` | `nat::get_router_falls_back_to_no_router` | done |
| `nat/upnp.go::upnpRouter` (`MapPort`/`UnmapPort`/`ExternalIP`/`getUPnPRouter`) | `nat::UpnpRouter` + `nat::get_upnp_router` (`igd-next`) | covered via `get_router` probe (no gateway in CI) | done |
| `nat/pmp.go::pmpRouter` / `getPMPRouter` | `nat::get_pmp_router` (stub → `None`) | n/a | **deferred** (see notes) |
| `nat/nat.go::Mapper` (`Map`/`retryMapPort`/`keepPortMapping`/`UnmapAllPorts`) | `nat::port_mapper::PortMapper` (`new`/`with_update_time`/`start`) | `nat::port_mapper_unmaps_on_shutdown` | done |

## Notes / provenance

- `tests/vectors/tls/staker.json` is reused from the M0.20 `tests/vectors/crypto/nodeid.json`
  vector (a Go-generated ECDSA P-256 staking cert DER + its NodeID). Same
  derivation path; no new extraction needed.
- `tests/vectors/message/signed_ip.json` is constructed from the documented
  `As16()(16) ‖ port_be(2) ‖ ts_be(8)` layout (`specs/05` §1.6, `specs/15` §4.1)
  for `1.2.3.4:9651 @ ts=1_600_000_000`. The IPv4 As16 form is the well-defined
  IPv4-mapped IPv6 (`00*10 ‖ ffff ‖ a.b.c.d`); cross-checked against the Packer
  layout. No Go toolchain was available in-sandbox to emit it from
  `network/peer/ip_test.go`, but the layout is deterministic and unambiguous.
- The RSA-1024 reject vector in `verifier::accepts_p256_rejects_others` is an
  openssl-generated cert DER embedded inline (the `ring` provider cannot generate
  RSA keys); it exercises the `modulus < 2048` reject branch.
- **NAT-PMP/PCP is deferred (M2.19).** `igd-next` covers only the UPnP IGD path;
  the Go reference probes NAT-PMP only as a *secondary* fallback after UPnP
  (`nat.getPMPRouter`), and CI has no PMP gateway either way, so `get_pmp_router`
  is a `None` stub. `get_router` therefore falls through UPnP → `NoRouter` with
  behaviour identical to Go on a PMP-less network. A real PMP probe (e.g. via
  `crab-nat`) is a follow-up; the `crate::Error::Nat`/`NoRouter` variants and the
  `PortMapper` keep-alive loop are PMP-router-agnostic, so wiring it later is
  additive.
- **`igd-next 0.17.1` is pinned directly in `crates/ava-network/Cargo.toml`**, not
  in the workspace `[workspace.dependencies]` table (it had no prior entry). It
  should be promoted to a workspace-level pin for consistency.
- The `nat` tests use a recording `MockRouter` (a `parking_lot::Mutex<Vec<…>>`
  call log) and `#[tokio::test(start_paused = true)]` (tokio `test-util`) so the
  `PortMapper` refresh interval is driven by the paused virtual clock — no
  wall-clock sleeps. `get_router_falls_back_to_no_router` waits the real 10s UPnP
  SOAP search timeout (no gateway in CI), matching Go's `soapRequestTimeout`.
