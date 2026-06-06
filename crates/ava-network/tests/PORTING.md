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

## Wave C — peer actor + handshake + dispatch runtime (M2.14–M2.18)

| Go source | Rust home | Test(s) | Status |
|---|---|---|---|
| `network/peer/peer.go::{Start,readMessages,writeMessages,sendNetworkMessages}` (3 goroutines) | `peer::peer::Peer::{spawn,run_read,run_write,run_net_messages}` (3 tokio tasks, `Arc<Peer>`) | `peer_actor::{write_task_sends_handshake_first, read_task_resets_deadline_and_drops_oversized, cancel_token_drains_tasks}` | done |
| `network/peer/peer.go` `MessageQueue` + `Send`/`StartSendGetPeerList`/`StartClose` + `onFinishHandshake`/`onClosed` | `peer::handle::{PeerHandle, PeerCommand}` (cmd `mpsc` + latch `CancellationToken`s) | covered by `peer_actor` / `handshake` | done |
| `network/peer/peer.go::handleHandshake` (all §1.4 disconnect reasons) | `peer::handshake::Peer::handle_handshake` | `handshake::{handshake_then_peerlist_completes, disconnect_reasons_close_the_connection, duplicate_handshake_closes}` | done |
| `network/peer/peer.go::handlePeerList` (finish handshake → `Connected`) | `peer::handshake::Peer::{handle_peer_list, finish_handshake}` → `ExternalHandler::connected` | `handshake::handshake_then_peerlist_completes` | done |
| `network/peer/peer.go::{handlePing,handlePong}` + uptime + RTT | `peer::handshake::Peer::{handle_ping, handle_pong}` + `peer::peer::Peer::send_ping` | `ping_pong::{ping_carries_uptime_and_pong_records_rtt, ping_uptime_over_100_closes, unsolicited_pong_closes}` | done |
| `network/peer/peer.go::shouldDisconnect` (compat re-check on tick, `specs/26` §3.1) | `peer::peer::Peer::{should_disconnect, is_compatible}` (injected clock) | `ping_pong::should_disconnect_on_clock_crossing_upgrade` | done |
| `utils/bloom/{read_filter,hasher,filter}.go` (Parse/Contains/Hash) | `network::bloom::{ReadFilter, hash}` (byte-exact) | `network::bloom` unit tests; `ip_gossip::peers_excludes_known_via_bloom` | done (byte-exact; Go-emitted cross-vector deferred to M2.22) |
| `network/ip_tracker.go` (track verified IPs, gossip exclusion) | `network::ip_tracker::IpTracker::{add_claimed_ip_port, peers, manually_track}` | `ip_gossip::{peers_excludes_known_via_bloom, claimed_ip_port_verified_before_track, bloom_salt_over_max_rejected}` | done (manual-track + verify; reconnect-backoff struct in `tracked_ip`, dial-loop integration in `net_impl`) |
| `network/tracked_ip.go` (reconnect backoff 1s→1m) | `network::tracked_ip::{TrackedIp, ClaimedIp}` | exercised via `net_impl` dial loop | done |
| `network/dialer/dialer.go::Dial` (30s timeout + 50 rps throttle) | `dialer::Dialer::dial` (hand-rolled token bucket) | `dialer` unit test + `network_dispatch::two_networks_connect_locally` | done |
| `network/network.go::{Dispatch,runTimers,dial,accept}` (#1/#2/#4) + conn-upgrade gate (#3) | `network::net_impl::NetworkImpl::{dispatch,run_accept,run_dialer,run_timers}` | `network_dispatch::{two_networks_connect_locally, start_close_drains_all_tasks}` | done |
| `network/network.go` peer sets + `Connected`/`Disconnected` bookkeeping | `network::peer_set::PeerSet` + `NetworkImpl::watch_peer` | `network_dispatch::two_networks_connect_locally` | done |
| `network/network.go::StartClose` (graceful drain, `specs/17` §4.3) | `NetworkImpl::start_close` (cancel token + `TaskTracker::close().wait()`) | `network_dispatch::start_close_drains_all_tasks` | done |

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

### Wave C notes / deferrals

- **Bloom filter home:** `utils/bloom` lives in `ava-utils` in the Go→Rust crate
  map, but is ported into `ava-network::network::bloom` (M2.17) so the handshake
  milestone is self-contained. The algorithm is byte-exact (SHA256 prefix hash,
  rotate-left-17 + seed XOR `Contains`), so a Go-built filter reads identically;
  a later refactor can hoist it into `ava-utils`. A Go-emitted bloom cross-vector
  is folded into the M2.22 differential.
- **Validator-weight / BLS-PoP re-check deferred:** `should_disconnect` re-runs
  the version-compatibility floor rule (the fork-boundary cut-over) but not the
  BLS proof-of-possession re-check, which needs the `validators.Manager` source
  (same `vdr_alloc=0` deferral as M2.12/M2.13). `txid_of_verified_bls_key` is
  reserved for it.
- **Subnet intersection deferred:** `finish_handshake` notifies the router on the
  primary network (`Id::default()`); the tracked-subnet intersection
  (`specs/05` §3.7) is refined when the subnet-set source lands.
- **Dialer throttle:** a hand-rolled token bucket (`parking_lot::Mutex`) is used
  instead of `governor`, the same dependency-minimizing choice the throttlers
  made — no loose-version crate added.
- **`net_impl` clock:** `network::testutil::TestNetwork` uses the real
  `SystemClock` (the loopback handshake needs `my_time` close to the signed-IP
  timestamp); the per-peer unit harness (`peer::testutil`) uses an injectable
  `TestClock` for the deterministic ping-interval / clock-crossing tests.
- **`differential::interop_handshake` (M2.22, `specs/05` §9.9 / `specs/26` §9.4 /
  `specs/02` §9) lives in the `ava-differential` crate**
  (`tests/differential/tests/interop_handshake.rs`), not here, so the recorded
  fallback can drive the *real* `ava-network` peer actor + `ava-message` codec
  end-to-end through the public `peer::testutil` surface (`TestPeerBuilder`,
  `read_one_frame`/`write_one_frame`) + `Peer::spawn` + `upgrader::
  node_id_from_cert_der`. **No new `ava-network` test helper was needed** — the
  M2.14–M2.21 testutil API already exposes everything (the only subtlety is that
  the peer-under-test verifies the signed IP against the cert passed to
  `Peer::spawn`, so the transcript ships the Go peer's leaf cert DER and the
  replay presents it).
  - **Two arms.** (1) `differential_interop_handshake` — the per-PR/offline
    **recorded fallback**: replays a Go-derived transcript and asserts the Rust
    side latches `finished_handshake` + fires `connected` exactly once with no
    protocol error. (2) `differential_interop_handshake_live` — the **live arm**
    behind the `interop` Cargo feature + `#[ignore]` (env `AVA_INTEROP_FUJI_ADDR`/
    `AVA_INTEROP_FUJI_NODE_ID`/`AVA_INTEROP_HOLD_SECS`); never runs in CI / this
    sandbox, a scheduled/nightly job dials a real Fuji peer.
  - **Transcript provenance (`tests/differential/tests/fixtures/fuji_transcript.bin`):**
    Go-emitted by a scratch program run against avalanchego
    `fb174e8925ba86e9ba5fd84eb4d6e5e8c23ffc11` (Go 1.25.9) using `staking.
    NewTLSCert()` + `network/peer.UnsignedIP.Sign` + `message.Creator.Handshake/
    PeerList`; the scratch program was deleted after capture. The committed proto
    bytes are the same byte-exact Go form as the `tests/vectors/message/
    {handshake,peerlist}.json` goldens, with a *real* signature/cert pair so the
    signed-IP verification passes. A true **live-captured** Fuji transcript is the
    gated/nightly follow-up (the live arm + cross-cutting X.15/X.22).
- **Deferred to later M2 tasks:** none remaining in the test surface — M2.20
  (`avalanche_network_*` metrics) and M2.21 (`prop::handshake_reaches_connected`)
  are complete; the live-Fuji arm of M2.22 is the only env-gated/nightly item.
- **M2.20b — live metric-increment wiring.** M2.20 registered the metric
  families (names/types/labels, parity-tested by `metrics::metric_names_match_go`)
  and left the `+1`/observe call sites as `// metrics:` notes. M2.20b wires them:
  - **Threading.** `PeerConfig` gained an `Option<PeerMetrics>` (set via
    `PeerConfig::with_peer_metrics`); `NetworkImpl` gained an `Option<Metrics>`
    (set via the new `NetworkImpl::new_with_metrics` — `NetworkImpl::new` stays
    signature-stable, defaulting metrics to `None`); `InboundMsgByteThrottler`
    gained `set_metrics`. `network::testutil::TestNetwork` now builds a real
    `prometheus::Registry`, registers both metric structs, attaches them, and
    exposes `registry()` so tests can `gather()`.
  - **Wired sites:** `times_connected`/`times_disconnected` (NetworkImpl
    `watch_peer`), `tls_conn_rejected` (inbound accept-path TLS upgrade `Err`
    arm — see finding below), per-peer `msgs`/`msgs_bytes`/`msgs_bytes_saved` +
    `msgs_failed_to_{parse,send}` (peer `handle_inbound`/`write_frame`), and
    `byte_throttler_inbound_remaining_{at_large,validator}_bytes` (pushed by the
    inbound byte throttler on every pool mutation).
  - **Test:** `tests/metrics_increments.rs` (2 tests) gathers the registry and
    asserts live changes — `live_increments_after_two_networks_connect` (covers
    `times_connected` + `msgs{io=sent}`/`msgs{io=received}` + the remaining-bytes
    gauge over a real two-network TLS handshake) and
    `tls_conn_rejected_increments_on_failed_upgrade` (raw-bytes client → failed
    inbound TLS upgrade).
  - **Finding — `tls_conn_rejected` placement.** Go increments this at the
    listener's inbound TLS upgrade-failure path; the rustls custom verifier
    callback (`peer::verifier`) has no `Metrics` handle, so the reject is metered
    one level up, at `NetworkImpl::handle_accepted`'s upgrade-`Err` arm where the
    rejection surfaces. This counts *all* inbound upgrade failures (a pure I/O
    failure mid-handshake also lands here), matching Go's behaviour where a
    failed `peer.upgrade` on the accept path bumps the counter.
  - **Deferred past M2.20b (with reason):** `num_useless_peerlist_bytes` — the
    `PeerList` ingest calls `IpTracker::add_claimed_ip_port`, which does not yet
    return a per-claim useless-byte delta; surfacing that is a larger IP-tracker
    change owned by the peer-list-gossip accounting milestone. Also still
    deferred: the `peers`/`peers_subnet` gauges (peer-set bookkeeping), the
    uptime gauges (uptime calculator / validator-set source), and the remaining
    throttler `awaiting_*` / outbound gauges (their runtime surfaces are not yet
    in this crate). All remain registered + name-parity-tested.
- **NAT-PMP/PCP is deferred (M2.19).** `igd-next` covers only the UPnP IGD path;
  the Go reference probes NAT-PMP only as a *secondary* fallback after UPnP
  (`nat.getPMPRouter`), and CI has no PMP gateway either way, so `get_pmp_router`
  is a `None` stub. `get_router` therefore falls through UPnP → `NoRouter` with
  behaviour identical to Go on a PMP-less network. A real PMP probe (e.g. via
  `crab-nat`) is a follow-up; the `crate::Error::Nat`/`NoRouter` variants and the
  `PortMapper` keep-alive loop are PMP-router-agnostic, so wiring it later is
  additive.
- **`igd-next` is pinned at the workspace level** (`[workspace.dependencies]`,
  `0.17.1`) and referenced as `{ workspace = true }` in this crate (promoted on
  merge).
- The `nat` tests use a recording `MockRouter` (a `parking_lot::Mutex<Vec<…>>`
  call log) and `#[tokio::test(start_paused = true)]` (tokio `test-util`) so the
  `PortMapper` refresh interval is driven by the paused virtual clock — no
  wall-clock sleeps. `get_router_falls_back_to_no_router` waits the real 10s UPnP
  SOAP search timeout (no gateway in CI), matching Go's `soapRequestTimeout`.
