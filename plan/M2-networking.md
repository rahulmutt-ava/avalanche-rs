# M2 — Networking Handshake Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Make a Rust node TLS-handshake a live Go node on Fuji, exchange byte-exact Handshake + PeerList over the length-framed protobuf p2p protocol, run ping/pong + IP signing/gossip, and stay connected — proving wire interop for the `ava-message` codec and the `ava-network` peer actor.
**Tier:** T2a — Wire
**Crates:** ava-message, ava-network
**Owning specs:** `05-networking-p2p.md` (PRIMARY), `15-serialization-and-wire-formats.md` §1.1/§3.1/§4.2 (framing/op-codes/zstd), `17-runtime-architecture.md` §2/§3/§4/§7 (task & channel topology, backpressure, peer actor, cancellation), `26-versioning-and-compatibility.md` §2/§3 (handshake compat rule, wire version string), `18-metrics-and-logging.md` §2.1–§2.3 (`avalanche_network_*` metric names), `02-testing-strategy.md` (TDD/golden/proptest/fuzz contracts), `00-overview-and-conventions.md` §3/§7/§9 (crate layout, runtime, zero-copy).
**Depends on (prior milestones):** M0 (ava-codec, ava-crypto, ava-types)
**Exit gate (named tests):** `golden::message_frames` (each op's wire bytes); `prop::frame_roundtrip` + fuzz `decode_never_overreads`; `prop::handshake_reaches_connected`; `differential::interop_handshake` (Rust node ↔ live Go node on Fuji: completes handshake, receives PeerList, holds connection ≥N s, no disconnect).

---

## Dependency map & parallel waves

Two crates land in dependency order; within each, message-codec work is independent of TLS/runtime work and can proceed in parallel.

```
M0 (ava-codec, ava-crypto[TLS staking cert + NodeID-from-cert], ava-types[Id, NodeId, bloom, constants])
   │
   ▼
Wave A  (ava-message — wire framing & codec, no network runtime)         [parallel within wave]
   ├─ M2.1  proto build + generated p2p module
   ├─ M2.2  Op enum + classification sets (UNREQUESTED_OPS / FAILED_TO_RESPONSE_OPS)
   ├─ M2.3  frame helpers (read_msg_len / write_msg_len, 2 MiB cap)
   ├─ M2.4  MsgBuilder marshal/unmarshal + recursive zstd packing (R4)
   ├─ M2.5  Builder API (OutboundMsgBuilder) — Handshake/Ping/Pong/GetPeerList/PeerList first
   └─ M2.6  golden::message_frames (TDD ENTRY POINT — Handshake op first) + prop::frame_roundtrip + fuzz decode_never_overreads

Wave B  (ava-network — TLS + identity; parallel to Wave A after M2.2)    [parallel within wave]
   ├─ M2.7  TLS configs (TLS1.3-only, mutual, no SNI) + crypto provider
   ├─ M2.8  custom cert verifiers (validate_leaf_public_key: P-256 / RSA rules)
   ├─ M2.9  Upgrader.upgrade → (NodeId, TlsStream, Certificate) + tls/ golden transcript
   └─ M2.10 IP signing (UnsignedIP::bytes layout, TLS sig over SHA256(bytes), BLS PoP) + signed-IP golden

Wave C  (ava-network — runtime; depends on Wave A + Wave B)
   ├─ M2.11 PeerConfig + Network/InboundHandler/ExternalHandler traits + version Compatibility wiring (26 §3)
   ├─ M2.12 Outbound MessageQueue (ThrottledMessageQueue + BlockingMessageQueue) + outbound byte throttler
   ├─ M2.13 inbound byte throttler + inbound-conn-upgrade throttler (05 §5)
   ├─ M2.14 Peer actor read/write/net-messages tasks + handshake state (depends 11/12)
   ├─ M2.15 handle_handshake (all disconnect reasons) → PeerList reply → finished_handshake → connected
   ├─ M2.16 ping/pong + uptime tracking + should_disconnect (re-check on tick)
   ├─ M2.17 IP-tracker + PeerList/GetPeerList gossip (bloom filter + salt) + ClaimedIpPort verify
   ├─ M2.18 dialer (timeout + dial throttle) + accept loop + Network::dispatch + runTimers
   ├─ M2.19 NAT (UPnP/NAT-PMP port mapper)
   ├─ M2.20 network metrics (avalanche_network_* names) + Connect-service enumeration note (R5)
   └─ M2.21 prop::handshake_reaches_connected (in-process duplex two-peer test)

Wave D  (differential / exit)
   ├─ M2.22 differential::interop_handshake (live Go Fuji node; CI-gated + recorded-transcript fallback)
   └─ M2.23 Milestone exit gate (buildable-&-green invariant)
```

> **R5 note:** the inter-node p2p protocol uses **no gRPC/Connect services** — it rides the raw framed TLS stream (`05` §3.8). The Connect services in the tree (`proposervm`, `xsvm`) and all `proto/` gRPC services are out of scope for M2; M2.20 only records the enumeration so later milestones (rpcchainvm, API) own them. The app-level SDK (`ava-network::p2p`, AppRequest/Gossip framing) is also deferred — M2 stops at handshake + peer-list + ping/pong.

---

## Progress & findings (Waves A + B complete — 2026-06-06)

**Wave A (`ava-message`, M2.1–M2.6) ✅ and Wave B (`ava-network` TLS/identity, M2.7–M2.10) ✅** landed via two parallel worktree agents; both crates green (36 tests total), clippy `-D warnings` clean, `avalanchers` binary still builds and reports `avalanchego/1.14.2`. Findings folded back into the specs:

1. **GetPeerList/PeerList compression (spec §1.3 corrected).** Go hard-codes only `Handshake`/`Ping`/`Pong` to `TypeNone`; `GetPeerList`/`PeerList` use the Creator's **default** (zstd). M2.5's "sent uncompressed" wording for those two was wrong — golden vectors capture the uncompressed byte-exact form, the live builder uses the zstd default (R4 decode-equivalence). Bulk-op builders (Put/Ancestors/PushQuery/App*/consensus/state-sync/simplex) are **left out entirely** (not `todo!()`-stubbed, per the no-`todo!()` lint rule) and deferred to their consuming engine milestones; the per-op compression table is wired via the Creator's default-compression field.
2. **`ip_addr` encoding.** Go's `Handshake.ip_addr` uses `Addr().AsSlice()` (4 bytes for IPv4, 16 for IPv6), while the *signed-IP* body uses `As16()`. `ava-types` has **no `Ip`/`as16()` type**; `ava-message::builder::ip_as16()` (IPv4→IPv4-mapped IPv6, always 16 B) is used for now and coincides with the Go fixtures. **TODO at M2.7+/M2.14:** add an `Ip`/`as16()` type to `ava-types` and have `ava-network` feed the real `MyIPPort` — a peer advertising a bare 4-byte IPv4 would otherwise diverge. (Recorded in `ava-message/tests/PORTING.md`.)
3. **`ava-network` deps.** Wave B's worktree branched before the M2-prep root pin, so it pinned rustls/tokio-rustls/ring locally; reconciled to `{ workspace = true }` on merge.
4. **`ava-crypto` API used (no crypto reinvented):** `staking::{new_cert_and_key_bytes, parse_certificate, node_id_from_cert, check_signature, Certificate, CertPublicKey}`, `bls::{LocalSigner, Signer, Signature, verify_pop}`. Notes: `NodeId` lives in `ava-types` (no `NodeId::from_cert` method — `ava-network::upgrader` wraps `ava_crypto::staking::node_id_from_cert`); there is **no standalone `validate_rsa_well_formed`** (folded into `parse_certificate`), so the verifier reimplements the RSA modulus/exponent policy to get the exact `CurveMismatch` vs `UnsupportedKeyType` error mapping. NodeID = `RIPEMD160(SHA256(leaf_DER))`, byte-identical to Go.
5. **`unused_crate_dependencies` idiom.** Crates that opt into `[lints] workspace = true` get `unused_crate_dependencies`, which false-positives per integration-test binary. Convention (already used across `ava-database`/`ava-codec`, now `ava-message`/`ava-network`): a file-level `#![allow(unused_crate_dependencies, clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]` on each integration-test file, plus genuine `#[cfg(test)]` unit tests in the lib so lib-test dev-deps are used.
6. **Golden-vector caveat:** `tests/vectors/message/signed_ip.json` was *constructed* from the documented `16 || port_be(2) || ts_be(8)` layout (no Go toolchain in-sandbox for that path), not Go-emitted; layout is unambiguous but a Go byte-for-byte cross-check is a follow-up. `tls/staker.json` and all `message/*.json` frame vectors ARE Go-derived/Go-emitted. Recorded in the crates' `PORTING.md`.
7. **`deadline` = nanoseconds (u64)**; zstd decode bounded to `MAX_MESSAGE_SIZE` (2 MiB) as the decompression-bomb guard the fuzz target relies on.

## Progress & findings (Wave C runtime — M2.14–M2.19 complete — 2026-06-06)

**M2.14–M2.18 (`ava-network` peer actor → dispatch runtime) ✅ and M2.19 (NAT port mapper) ✅** landed via two parallel worktree agents (the M2.14–M2.18 chain is strictly sequential — peer actor → handshake → ping/pong → gossip → dispatch, all on `peer/peer.rs` + `network/net_impl.rs` — so one agent did all five in order; M2.19 NAT is a self-contained `nat/` module, so it ran in parallel). After merge: **`ava-network` green at 51 tests**, `cargo clippy --all-targets -- -D warnings` clean, `avalanchers` binary still builds. Headline: `network_dispatch::two_networks_connect_locally` brings up two real `NetworkImpl` instances on loopback, dials over real TLS 1.3, completes the byte-exact Handshake+PeerList exchange, and reaches `connected` end-to-end. Findings:

1. **`PeerConfig` field set grew (spec §3.1 / M2.11 note resolved).** M2.14 needed the full collaborator set, so `PeerConfig` now carries `identity`, `my_ip`, `my_version`, `my_tracked_subnets`, `my_supported_acps`/`objected_acps`, `creator`, `router`, `version_compatibility`, the three throttlers, `ip_signer`, `ip_tracker`, and an injected `clock` — i.e. the throttlers/ip_signer/clock the M2.11 note anticipated, plus identity/ip/version/subnet/ACP inputs. `PeerConfig::new` changed from the M2.11 5-arg stub; the only caller (`tests/traits.rs`) builds it via `peer::testutil::TestPeerBuilder`. Spec `05` §3.1's `PeerConfig` field list updated to match.
2. **Bloom filter ported into `ava-network::network::bloom`, not `ava-utils`.** Byte-exact port of Go `utils/bloom` (SHA256-prefix hash, rotate-left-17 + seed-XOR `Contains`) so a Go-built filter reads identically. Go's natural home is `ava-utils`; a later refactor can hoist it. A Go-emitted bloom cross-vector is folded into M2.22. (Recorded in spec `05` §3.5.)
3. **`should_disconnect` re-checks the version-compat floor only**, not the BLS-PoP re-check (needs the `validators.Manager` source — same `vdr_alloc=0` deferral as M2.12/M2.13). `txid_of_verified_bls_key` is reserved for it. The compat re-check reads `Compatibility`'s public fields against the injected `Clock::now_system()` so the clock-crossing test is deterministic (no wall-clock).
4. **`finish_handshake` notifies the router on the primary network** (`Id::default()`); the tracked-subnet intersection (`05` §3.7) is refined when the subnet-set source lands.
5. **Dialer rate limiter is a hand-rolled token bucket** (`parking_lot::Mutex`), not `governor` — the same dependency-minimizing choice the throttlers (M2.12/M2.13) made. `governor` stays out of the workspace.
6. **`handshake.rs` + a peer test-support module are introduced at M2.14** (not M2.15) — the actor can't compile without the inbound-dispatch handlers; handlers were stubbed at M2.14 and filled at M2.15/M2.16/M2.17. (Plan file lists corrected below.)
7. **NAT: `igd-next 0.17.1`** (UPnP-only) promoted to a **workspace dependency** on merge. NAT-PMP/PCP is a `get_pmp_router() -> None` stub: Go probes PMP only as a secondary fallback after UPnP, and CI has no gateway, so `get_router` falls through UPnP → `NoRouter` — behaviourally identical to a PMP-less network. The `Error::{Nat, NoRouter}` variants and the `PortMapper` keep-alive loop are PMP-agnostic, so a real PMP probe later is additive. `PortMapper` ports Go `nat.Mapper` (re-map every `MAP_TIMEOUT=30m`, `MAX_REFRESH_RETRIES=3`, unmap on `CancellationToken`); its tests use a recording mock router + paused tokio clock. The optional advertised-IP `updateIP` side of Go's `Mapper.Map` is **deferred to `ava-node`/`network` wiring** (`specs/17` task #23 attributes it there), since it couples to a network-owned advertised-IP atomic that doesn't exist in this crate yet.

## Progress & findings (Wave C tail — M2.20 + M2.21 complete — 2026-06-06)

**M2.20 (`avalanche_network_*` metrics + R5 Connect-service note) ✅ and M2.21 (`prop::handshake_reaches_connected`) ✅** landed via two parallel worktree agents (independent file sets — M2.20 adds `metrics.rs`/`peer/metrics.rs`/`docs/connect-services.md`; M2.21 adds `tests/prop_handshake.rs` — so they ran concurrently). After merge: **`ava-network` green at 53 tests** (51 prior + `metrics::metric_names_match_go` + `prop_handshake::handshake_reaches_connected`), `cargo clippy -p ava-network --all-targets -- -D warnings` clean, `avalanchers` binary still builds and reports `avalanchego/1.14.2`. The only merge friction was an additive `Cargo.lock` conflict (both crates added a dep to ava-network's list: `prometheus` ∥ `proptest`) resolved by keeping both in sorted order. Findings:

1. **Metrics are registered with BARE family names** (`peers`, `times_connected`, `msgs{io,op,compressed}`, `byte_throttler_inbound_remaining_at_large_bytes`, …); the node-level `PrefixGatherer` (owned by `ava-api`, spec `18` §1.1/§1.2) prepends `avalanche_network_`. This matches the `ava-database::meterdb` precedent (registers bare `calls`/`duration`/`size`). So `tests/metrics.rs` asserts bare names; the full `avalanche_network_*`-prefixed golden is an `ava-api`/node-level concern (spec `18` §3) **deferred** to the node milestone. (Spec `05`/`18` already place prefixing at the gatherer — no spec change; confirmed correct Go-parity placement.)
2. **Live increments are registration-only at M2.20.** The metric families + exact names + family types + label keys (the parity surface) are complete and constructible, with convenience observe methods (`observe_connected`, `observe_tls_conn_rejected`, `observe_sent`/`observe_received`, `set_inbound_byte_remaining`, …) so future wiring is one-line. The actual `+1` call sites (verifier/upgrader reject path → `tls_conn_rejected`; peer read/write tasks → `msgs*`; byte-throttler pools → `remaining_*`) were left as documented `// metrics:` notes because threading a metrics handle through the M2.13/M2.14 constructors is the risky refactor M2.20 was scoped to avoid. **Wiring the live increments is a follow-up** (fold into M2.23 exit-gate cleanup or a dedicated M2.20b) — no existing code path was modified, so no existing test was touched.
3. **Averager rows expanded to `_count`/`_sum` Counter pairs.** Spec `18` §2.2's `round_trip`/`clock_skew` "Avg" rows become the `_count`+`_sum` Counter pair per the §2 averager note. The §2.1 rows whose *name* contains "average" (`node_uptime_weighted_average`, `peer_connected_duration_average`) are plain gauges, not averagers — matched as `Gauge`.
4. **Integer gauges use `IntGauge`** (peers/tracked/throttler byte counts) — same Prometheus `gauge` family type on the wire, so no parity-surface change vs Go's float `Gauge`; indistinguishable at text-exposition level.
5. **`Error::Metrics(String)`** variant added to `ava-network`'s error enum for registry-registration failures (ava-network lacked meterdb's `Error::Other`).
6. **`unused_crate_dependencies` lib-test gotcha (M2.21, reusable for M2.22+ and any per-crate proptest suite).** Because `ava-network` has `#[cfg(test)]` modules in `src/`, its "lib test" build links every dev-dependency — so a dev-dep used *only* from a `tests/` integration target (here `proptest`) trips `unused_crate_dependencies` under `-D warnings`. Fix: a `#[cfg(test)] use <crate> as _;` somewhere in `src/` (M2.21 put it in `peer/testutil.rs`). Crates without in-`src` `#[cfg(test)]` modules (e.g. `ava-codec`) don't hit this.
7. **M2.21 reused the existing `PeerHarness`/`HandshakeOverrides` infra unchanged** — every valid and single-§1.4-violation parameter (network_id, ±60s clock skew, version floor, ≤16/>16 subnets, disjoint/overlapping ACPs, port, corrupt IP sig, bloom salt ≤32/>32) was already expressible. 64 cases × {valid arm + violation arm} per run, `FileFailurePersistence::SourceParallel("proptest-regressions")` (committed empty corpus dir via `.gitkeep`, matching `ava-codec`).

**Next:** Wave D — **M2.22** (`differential::interop_handshake`, live Fuji + recorded fallback) depends on M2.21 (done) + the cross-cutting harness X; **M2.23** (milestone exit gate) depends on all prior M2 tasks. Note for M2.23: fold in the M2.20 live-increment wiring (finding 2) so the metrics are not merely registered but observed before the milestone closes.

---

## Tasks

### Task M2.1: proto build + generated `p2p` module ✅ COMPLETED
**Crate:** ava-message  ·  **Depends on:** M0 (ava-types)  ·  **Spec:** `05` §2.1, `15` §3.1/§5
**Files:** `crates/ava-message/Cargo.toml`, `crates/ava-message/build.rs`, `crates/ava-message/proto/p2p/p2p.proto` (vendored verbatim from the Go tree), `crates/ava-message/src/lib.rs`, `crates/ava-message/src/proto.rs` (re-export `pub mod p2p`).
- [ ] **Step 1 — Red:** add `crates/ava-message/tests/proto_smoke.rs` with `#[test] fn proto_module_has_message_oneof()` that constructs `p2p::Message { message: Some(p2p::message::Message::Ping(p2p::Ping { uptime: 0 })) }` and asserts `prost::Message::encoded_len(&m) > 0`. Real signature; it references generated types that do not yet exist.
- [ ] **Step 2 — Confirm red:** `cargo test -p ava-message --test proto_smoke` → fails to compile (`unresolved import p2p`). Failure is the missing generated module.
- [ ] **Step 3 — Green:** vendor `proto/p2p/p2p.proto` (the §3.1 schema: `Message` oneof with the exact tags in `15` §3.1, `reserved 1; reserved 37;`). In `build.rs` run `prost-build`/`tonic-build` configured with `.bytes(["."])` so `bytes`/`repeated bytes` map to `bytes::Bytes` (`15` §5, `00` §9); emit into `OUT_DIR`; `proto.rs` does `pub mod p2p { include!(concat!(env!("OUT_DIR"), "/p2p.rs")); }`. Generated code is NOT committed (`00` decision 8). License header on hand-written files; `#![forbid(unsafe_code)]` in `lib.rs`.
- [ ] **Step 4 — Confirm green:** `cargo test -p ava-message --test proto_smoke` passes; `cargo build -p ava-message` clean.
- [ ] **Step 5 — Commit:** `ava-message: vendor p2p.proto + prost build (bytes::Bytes mapping)`.

### Task M2.2: `Op` enum + classification sets ✅ COMPLETED
**Crate:** ava-message  ·  **Depends on:** M2.1  ·  **Spec:** `05` §1.2/§2.2
**Files:** `crates/ava-message/src/ops.rs`, `crates/ava-message/src/lib.rs` (`pub mod ops`).
- [ ] **Step 1 — Red:** `crates/ava-message/tests/ops_table.rs`: `#[test] fn op_values_and_strings_match_go()` asserts `Op::Ping as u8 == 0`, `Op::Simplex as u8 == 35`, `Op::Handshake.as_str() == "handshake"`, `Op::GetPeerList.as_str() == "get_peerlist"`, and that `Op::of(&p2p::message::Message::Handshake(_)) == Ok(Op::Handshake)`; plus `unrequested_ops()` and `failed_to_response_ops()` contain the exact members from `05` §1.2 (e.g. `FAILED_TO_RESPONSE_OPS[GetFailed] == Put`, `QueryFailed == Chits`, `AppError == AppResponse`).
- [ ] **Step 2 — Confirm red:** `cargo test -p ava-message --test ops_table` → fails (`ops` module / `Op` missing).
- [ ] **Step 3 — Green:** implement `#[repr(u8)] enum Op` with the exact `iota` ordering from `05` §2.2; `as_str()` returns the exact `String()` names; `Op::of(&p2p::message::Message) -> Result<Op, Error>` mirrors `ToOp` (oneof variant → Op); `pub fn unrequested_ops() -> &'static HashSet<Op>` and `pub fn failed_to_response_ops() -> &'static HashMap<Op, Op>` (via `LazyLock`) reproduce `05` §1.2 verbatim. Add `ava-message::Error` (`thiserror`) with `UnknownOp` to start.
- [ ] **Step 4 — Confirm green:** `cargo test -p ava-message --test ops_table` passes.
- [ ] **Step 5 — Commit:** `ava-message: Op enum + UNREQUESTED_OPS/FAILED_TO_RESPONSE_OPS`.

### Task M2.3: frame helpers (length prefix, 2 MiB cap) ✅ COMPLETED
**Crate:** ava-message  ·  **Depends on:** M2.1  ·  **Spec:** `05` §1.1/§2.3, `15` §4.2
**Files:** `crates/ava-message/src/frame.rs`, `crates/ava-message/src/error.rs`, `crates/ava-message/src/lib.rs`.
- [ ] **Step 1 — Red:** `crates/ava-message/tests/frame.rs`: `#[test] fn read_msg_len_be_and_cap()` asserts `read_msg_len([0,0,0,4], MAX_MESSAGE_SIZE) == Ok(4)`, `read_msg_len([0,0,0,0], MAX) == Ok(0)`, and `read_msg_len([0,0x20,0,1], MAX)` (= 2 MiB + 1) is `Err(Error::MaxMessageLengthExceeded { .. })`; `write_msg_len` of `MAX_MESSAGE_SIZE + 1` errors. Assert `MAX_MESSAGE_SIZE == 2 * 1024 * 1024`.
- [ ] **Step 2 — Confirm red:** `cargo test -p ava-message --test frame` → fails (missing `frame` module).
- [ ] **Step 3 — Green:** implement `pub const MAX_MESSAGE_SIZE: u32 = 2 * 1024 * 1024;`, `pub fn write_msg_len(buf: &mut BytesMut, len: u32) -> Result<()>` (BE u32, err if `> MAX`), `pub fn read_msg_len(b: [u8; 4], max: u32) -> Result<u32>` (BE u32, err if `> max`) — byte-exact with `msg_length.go`. Extend `Error` with `MaxMessageLengthExceeded { len, max }` and `InvalidMessageLength`.
- [ ] **Step 4 — Confirm green:** `cargo test -p ava-message --test frame` passes.
- [ ] **Step 5 — Commit:** `ava-message: 4-byte BE frame helpers + 2 MiB cap`.

### Task M2.4: `MsgBuilder` marshal/unmarshal + recursive zstd packing (R4) ✅ COMPLETED
**Crate:** ava-message  ·  **Depends on:** M2.2, M2.3  ·  **Spec:** `05` §1.3/§2.3, `15` §3.1/§4.2 (R4)
**Files:** `crates/ava-message/src/codec.rs`, `crates/ava-message/src/lib.rs`.
- [ ] **Step 1 — Red:** `crates/ava-message/tests/codec_roundtrip.rs`: `#[test] fn marshal_unmarshal_uncompressed()` builds a `p2p::Message::Ping`, `marshal(&m, Compression::None)` → bytes, `unmarshal(&bytes)` → `(msg, saved, op)` with `op == Op::Ping`, `saved == 0`, msg equal. `#[test] fn marshal_unmarshal_zstd_roundtrip()` builds a large `Put`-like message, `marshal(&m, Compression::Zstd)` → outer `Message` whose only set field is `compressed_zstd` (assert by re-decoding outer with prost and checking `bytes_saved_compression < 0` i.e. compressed), then `unmarshal` recovers the inner message (R4: decode-equivalence, not byte-equality).
- [ ] **Step 2 — Confirm red:** `cargo test -p ava-message --test codec_roundtrip` → fails (`MsgBuilder`/`codec` missing).
- [ ] **Step 3 — Green:** implement `MsgBuilder { zstd, max_message_timeout, metrics }`, `marshal(&self, m, Compression) -> Result<(Bytes, i64, Op)>` (marshal inner; if Zstd, `zstd` window = `MAX_MESSAGE_SIZE`, build outer `Message{compressed_zstd}` and marshal that; compute `bytes_saved = inner_len - outer_len`), `unmarshal(&self, &[u8]) -> Result<(p2p::Message, i64, Op)>` (decode outer; if `compressed_zstd` non-empty → `zstd_decompress` bounded to `MAX_MESSAGE_SIZE` then decode inner — guard decompressed length to prevent over-read). Add `Compression { None, Zstd }`, `OutboundMessage`/`InboundMessage` (`05` §2.2) with the `OnFinished` RAII guard stub. Extend `Error::UnknownCompressionType`. Reuse one zstd context (perf note `15` §8). Vendor a `flate2` gzip *decode-only* tolerance path but never produce it (`05` §1.3).
- [ ] **Step 4 — Confirm green:** `cargo test -p ava-message --test codec_roundtrip` passes.
- [ ] **Step 5 — Commit:** `ava-message: MsgBuilder marshal/unmarshal + recursive zstd packing`.

### Task M2.5: Builder API (`OutboundMsgBuilder`) — handshake-class first ✅ COMPLETED
**Crate:** ava-message  ·  **Depends on:** M2.4  ·  **Spec:** `05` §2.4
**Files:** `crates/ava-message/src/builder.rs`, `crates/ava-message/src/lib.rs`.
- [ ] **Step 1 — Red:** `crates/ava-message/tests/builder.rs`: `#[test] fn build_handshake_sets_fields()` calls `Creator::handshake(...)` with concrete args (network_id, my_time, ip:SocketAddr, client name/major/minor/patch, upgrade_time, ip_signing_time, tls_sig, bls_sig, tracked_subnets, supported_acps, objected_acps, known_peers filter+salt, all_subnets) → `OutboundMessage`, then `unmarshal(&out.bytes)` recovers a `Handshake` whose `ip_addr` is the 16-byte `As16()` form, `ip_port` matches, `client` has the version triple, `op == Op::Handshake`, `bypass_throttling == false`. Add tests `build_ping_sets_uptime`, `build_pong`, `build_get_peer_list`, `build_peer_list_bypass_throttling_true`.
- [ ] **Step 2 — Confirm red:** `cargo test -p ava-message --test builder` → fails (`Creator`/`builder` missing).
- [ ] **Step 3 — Green:** implement `trait OutboundMsgBuilder` with the `05` §2.4 signatures and `Creator` holding an `Arc<MsgBuilder>`. Implement at minimum `handshake`, `ping(uptime)`, `pong`, `get_peer_list(filter,salt,all_subnets)`, `peer_list(peers,bypass)`. Copy the per-op compression decision from Go (`05` §1.3, corrected): only handshake/ping/pong hard-coded **uncompressed** (`TypeNone`); get_peerlist/peerlist use the Creator's **default** compression (zstd); bulk ops (Put/Ancestors/PushQuery/App\*) zstd — stub the bulk ops as `todo!()`/`#[allow(dead_code)]` signatures deferred to later milestones, but wire the compression flag table now. Encode `ip_addr` via `Ip::as16()` (`ava-types`), `deadline` as ns u64.
- [ ] **Step 4 — Confirm green:** `cargo test -p ava-message --test builder` passes.
- [ ] **Step 5 — Commit:** `ava-message: OutboundMsgBuilder handshake/ping/pong/peerlist + per-op compression table`.

### Task M2.6: `golden::message_frames` + `prop::frame_roundtrip` + fuzz `decode_never_overreads` (TDD ENTRY POINT) ✅ COMPLETED
**Crate:** ava-message  ·  **Depends on:** M2.5  ·  **Spec:** `05` §9 (1,2), `15` §7, `02` §4/§6/§8
**Files:** `crates/ava-message/tests/golden.rs`, `crates/ava-message/tests/prop_frame.rs`, `crates/ava-message/proptest-regressions/` (committed), `crates/ava-message/fuzz/Cargo.toml`, `crates/ava-message/fuzz/fuzz_targets/decode_never_overreads.rs`, `tests/vectors/message/` (per-op `len_be || proto_bytes` fixtures captured from a Go node), `crates/ava-message/tests/PORTING.md`.
- [ ] **Step 1 — Red:** start with the **Handshake op** vector. Capture `tests/vectors/message/handshake.json` (`{input_fields, hex_frame}`) from the Go `message/messages_test.go` path (`02` §10 extract program). Write `golden::message_frames`: `#[test] fn message_frames()` iterates `tests/vectors/message/*.json`, rebuilds each op via `Creator`, and asserts `frame(out.bytes) == hex::decode(vector.hex_frame)` **byte-identical** for uncompressed ops (Handshake, Ping, Pong, GetPeerList, PeerList, Get, Chits, AppRequest), and for zstd ops asserts only **cross-decodability** (`unmarshal(go_frame) == our inner message`) per R4. Write `prop::frame_roundtrip` (`proptest!`): for an arbitrary `Op`/field set, `build → marshal → frame → read_msg_len → unmarshal → unwrap` is identity, both compressions. Write the fuzz target `decode_never_overreads`: `fuzz_target!(|data: &[u8]| { let _ = MsgBuilder::default().unmarshal(data); })` — must never panic / never read past the buffer / never allocate `> MAX_MESSAGE_SIZE`.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-message -E 'test(message_frames)'` fails on a byte diff for `handshake.json` (the right reason — bytes not yet exact); `cargo nextest run -p ava-message -E 'test(frame_roundtrip)'` fails until codec lands; `cargo +nightly fuzz run decode_never_overreads -- -runs=10000` is wired.
- [ ] **Step 3 — Green:** fix any field-ordering / default-elision / `As16()` / deadline-units mismatch surfaced by the Handshake golden until bytes are identical; extend coverage to the remaining per-op vectors. Add `FileFailurePersistence::SourceParallel("proptest-regressions")` (`02` §4.1). Add `decode_never_overreads` to the fuzz smoke list. Seed `PORTING.md` rows for `messages_test.go` cases.
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-message` all green; `cargo +nightly fuzz run decode_never_overreads -- -runs=100000` no crash.
- [ ] **Step 5 — Commit:** `ava-message: golden::message_frames + prop::frame_roundtrip + fuzz decode_never_overreads`.

### Task M2.7: TLS configs (TLS 1.3-only, mutual, no SNI) ✅ COMPLETED
**Crate:** ava-network  ·  **Depends on:** M0 (ava-crypto: staking cert/key), M2.2  ·  **Spec:** `05` §1.6/§4.1/§4.2
**Files:** `crates/ava-network/Cargo.toml`, `crates/ava-network/src/lib.rs`, `crates/ava-network/src/peer/tls_config.rs`, `crates/ava-network/src/error.rs`.
- [ ] **Step 1 — Red:** `crates/ava-network/tests/tls_config.rs`: `#[test] fn configs_are_tls13_only_and_mutual()` builds `server_config()` and `client_config()` from a generated staking identity and asserts both restrict to `&[&rustls::version::TLS13]`, the server config requires a client cert (mutual), and neither sets ALPN. (Inspect via the builder return type / a thin accessor.)
- [ ] **Step 2 — Confirm red:** `cargo test -p ava-network --test tls_config` → fails (module missing).
- [ ] **Step 3 — Green:** add deps `rustls`, `tokio-rustls`, `rcgen`, `x509-parser`, `ring`/`aws-lc-rs` (`05` §4.1). Implement `fn server_config(identity) -> Arc<ServerConfig>` and `fn client_config(identity) -> Arc<ClientConfig>`: TLS 1.3 only, default provider suite list **unmodified** (the three TLS1.3 suites match Go), our staking cert+key as the presented cert in both directions, no ALPN, no SNI verification. Verifiers are placeholders here (next task wires the real ones). `#![forbid(unsafe_code)]` except the rustls `danger` verifier wrapper module is `#[allow]`-annotated per `00` §297.
- [ ] **Step 4 — Confirm green:** `cargo test -p ava-network --test tls_config` passes.
- [ ] **Step 5 — Commit:** `ava-network: TLS1.3-only mutual rustls server/client configs`.

### Task M2.8: custom cert verifiers (leaf-key policy) ✅ COMPLETED
**Crate:** ava-network  ·  **Depends on:** M2.7, M0 (ava-crypto: RSA well-formed check)  ·  **Spec:** `05` §1.6/§4.4/§4.5
**Files:** `crates/ava-network/src/peer/verifier.rs`, `crates/ava-network/src/peer/tls_config.rs` (wire verifiers in).
- [ ] **Step 1 — Red:** `crates/ava-network/tests/verifier.rs`: `#[test] fn accepts_p256_rejects_others()` — `validate_leaf_public_key(der)` returns `Ok` for a P-256 ECDSA staking cert, `Err(Error::CurveMismatch)` for a P-384 ECDSA cert, `Err(Error::UnsupportedKeyType)` for an Ed25519 cert, and applies `validate_rsa_well_formed` (reject modulus `< 2048`).
- [ ] **Step 2 — Confirm red:** `cargo test -p ava-network --test verifier` → fails.
- [ ] **Step 3 — Green:** implement `validate_leaf_public_key(&CertificateDer) -> Result<(), rustls::Error>` (`05` §4.5: parse via `x509-parser`; EC ⇒ require P-256; RSA ⇒ `staking::validate_rsa_well_formed` from `ava-crypto`/M0; else reject). Implement `AvaClientCertVerifier` (server side: `client_auth_mandatory()==true` == `RequireAnyClientCert`, `verify_client_cert` runs the policy then `assertion()`, `verify_tls13_signature` delegates to `rustls::crypto::verify_tls13_signature` — real key-possession proof) and `AvaServerCertVerifier` (client side mirror). Wire both into the configs from M2.7. Add `Error::{NoCertsSent, EmptyCert, CurveMismatch, UnsupportedKeyType}`.
- [ ] **Step 4 — Confirm green:** `cargo test -p ava-network --test verifier` passes.
- [ ] **Step 5 — Commit:** `ava-network: leaf-key cert verifiers (P-256/RSA policy, no CA chain)`.

### Task M2.9: `Upgrader` + node-id-from-cert + `tls/` golden transcript ✅ COMPLETED
**Crate:** ava-network  ·  **Depends on:** M2.8, M0 (ava-crypto: `NodeId::from_cert`)  ·  **Spec:** `05` §1.6/§4.3, `05` §9 (TLS interop unit test)
**Files:** `crates/ava-network/src/peer/upgrader.rs`, `crates/ava-network/tests/tls_handshake.rs`, `crates/ava-network/tests/golden_tls.rs`, `tests/vectors/tls/` (node-id-from-cert handshake transcript: known DER → known NodeID).
- [ ] **Step 1 — Red:** `crates/ava-network/tests/tls_handshake.rs`: `#[tokio::test] async fn loopback_mutual_tls_derives_node_id()` runs a rustls server config + client config over `tokio::io::duplex` (or a loopback `TcpListener`), completes the TLS 1.3 handshake, and on both sides `upgrade()` returns `(NodeId, TlsStream, Certificate)` where each side's derived peer `NodeId == NodeId::from_cert(peer_cert)`. Add `rejects_non_p256` (handshake fails). `golden_tls.rs`: `#[test] fn node_id_from_cert_golden()` loads `tests/vectors/tls/staker.json` (`{cert_der_hex, node_id}`) and asserts `NodeId::from_cert(parse(der)).to_string() == node_id` (RIPEMD160(SHA256(DER)), `05` §1.6).
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-network -E 'test(loopback_mutual_tls_derives_node_id) + test(node_id_from_cert_golden)'` → fails (`upgrader` missing).
- [ ] **Step 3 — Green:** implement `Upgrader { side, acceptor, connector }` and `async fn upgrade(&self, tcp) -> Result<(NodeId, TlsStream<TcpStream>, Certificate)>` per `05` §4.3 (accept/connect by side; extract leaf via `peer_certificates().first()`; `staking::parse_certificate`; `NodeId::from_cert`). Capture the `tests/vectors/tls/staker.json` vector from the Go `staking`/`ids.NodeIDFromCert` test path (`02` §10).
- [ ] **Step 4 — Confirm green:** both tests pass.
- [ ] **Step 5 — Commit:** `ava-network: TLS Upgrader + NodeID-from-cert + tls/ golden`.

### Task M2.10: IP signing (`UnsignedIP`/`SignedIp`) + signed-IP golden ✅ COMPLETED
**Crate:** ava-network  ·  **Depends on:** M2.9, M0 (ava-crypto: TLS-key sign, BLS PoP)  ·  **Spec:** `05` §1.6 (IP signing) / §3.5, `05` §9 (4), `15` §4.1 (signed-IP linear codec)
**Files:** `crates/ava-network/src/peer/ip.rs`, `crates/ava-network/src/peer/ip_signer.rs`, `crates/ava-network/tests/signed_ip.rs`, `tests/vectors/message/signed_ip.json`.
- [ ] **Step 1 — Red:** `crates/ava-network/tests/signed_ip.rs`: `#[test] fn unsigned_ip_bytes_layout()` asserts `UnsignedIp{ip, port, timestamp}.bytes()` equals `ip.as16() (16) || port.to_be_bytes() (2) || timestamp.to_be_bytes() (8)` against a golden in `signed_ip.json`. `#[test] fn signed_ip_verify_roundtrip()` signs with a staking key, `SignedIp::verify(cert, max_timestamp)` is `Ok`; a `timestamp > now+60s` → `Err(Error::TimestampTooFarInFuture)`; a tampered sig → `Err(Error::InvalidTlsSignature)`.
- [ ] **Step 2 — Confirm red:** `cargo test -p ava-network --test signed_ip` → fails.
- [ ] **Step 3 — Green:** implement `UnsignedIp::bytes()` (the `16 || port_be || ts_be` Packer layout, `05` §1.6), TLS signature over `SHA256(bytes)` and BLS PoP over raw `bytes`; `SignedIp::verify` (timestamp ≤ now+60s; valid TLS sig from peer cert). `IpSigner` caches the current `SignedIp` in an `arc_swap::ArcSwap`, re-signing on IP change (`05` §3.5). Add `Error::{TimestampTooFarInFuture, InvalidTlsSignature}`. Capture `signed_ip.json` from the Go `ip`/`ip_signer` test path.
- [ ] **Step 4 — Confirm green:** `cargo test -p ava-network --test signed_ip` passes.
- [ ] **Step 5 — Commit:** `ava-network: IP signing (UnsignedIP bytes + SignedIp verify) + golden`.

### Task M2.11: `PeerConfig` + Network/Inbound/External traits + version Compatibility wiring ✅ COMPLETED
**Crate:** ava-network  ·  **Depends on:** M2.5, M2.10, M0 (ava-version via `26`)  ·  **Spec:** `05` §3.1/§3.6, `26` §3
> **Done (2026-06-06):** `router.rs` (object-safe `InboundHandler`/`ExternalHandler`, `AppVersion = ava_version::Application`), `config.rs` (`PeerConfig`), `network/mod.rs` (`Network` trait + `SendConfig`/`GossipConfig`/`Allower`/`PeerInfo`/`UptimeResult`); `tests/traits.rs` green (2 tests). **Findings:** (a) the `Network` surface is a pure trait so no `todo!()` bodies are needed (satisfies the no-`todo!()` lint) — the M2.18 `NetworkImpl` fills it in. (b) `PeerConfig` was kept lean: it carries `network_id`/`my_node_id`/`creator`/`router`/`version_compatibility`; the **throttlers (M2.12/M2.13), `IpSigner`+`Clock` (M2.14), and metrics registry (M2.20)** are added to the struct as those tasks land (documented in the struct doc) rather than forward-declared with placeholder types — avoids a churn of stub types. (c) `ava-network` now depends on `ava-message`+`ava-version` (Wave B was TLS-only). (d) there is **no `ava-version::get_compatibility`** free fn — use `Compatibility::new(current, min_after_upgrade, min_compatible, upgrade_time)`.
**Files:** `crates/ava-network/src/config.rs`, `crates/ava-network/src/network/mod.rs`, `crates/ava-network/src/router.rs` (traits), `crates/ava-network/src/lib.rs`.
- [ ] **Step 1 — Red:** `crates/ava-network/tests/traits.rs`: `#[test] fn inbound_handler_object_safe()` builds a no-op `struct TestHandler` implementing `ExternalHandler` (and thus `InboundHandler`) and stores it as `Arc<dyn ExternalHandler>` — compile-level contract that the traits in `05` §3.6 are object-safe with the exact method signatures. `#[test] fn compatibility_floor_rule()` asserts `PeerConfig`'s `Arc<Compatibility>` rejects a peer below the floor and accepts an equal-version peer (delegates to `ava-version::Compatibility::compatible`, `26` §3 — newer-major reject, fork-boundary cut-over).
- [ ] **Step 2 — Confirm red:** `cargo test -p ava-network --test traits` → fails (traits/config missing).
- [ ] **Step 3 — Green:** define `#[async_trait] trait InboundHandler { async fn handle_inbound(&self, ctx: &CancellationToken, msg: InboundMessage); }` and `trait ExternalHandler: InboundHandler { fn connected(&self, NodeId, &AppVersion, Id); fn disconnected(&self, NodeId); }` (`05` §3.6). Define `PeerConfig` (creator `Arc<Creator>`, throttlers (forward-declared), router `Arc<dyn ExternalHandler>`, `version_compatibility: Arc<Compatibility>` via `ava-version::get_compatibility`, IP signer, clock, metrics) and the `Network` trait surface (`05` §3.1) with `dispatch`/`start_close`/`manually_track`/`peer_info`/`node_uptime`/`send`/`gossip` (bodies may be `todo!()` until M2.18). Wire `26` §3 compat rule reference.
- [ ] **Step 4 — Confirm green:** `cargo test -p ava-network --test traits` passes.
- [ ] **Step 5 — Commit:** `ava-network: PeerConfig + InboundHandler/ExternalHandler traits + compat wiring`.

### Task M2.12: outbound `MessageQueue` + outbound byte throttler ✅ COMPLETED
**Crate:** ava-network  ·  **Depends on:** M2.11  ·  **Spec:** `05` §3.3/§5, `17` §3 (peer outbound queue row)
**Files:** `crates/ava-network/src/peer/message_queue.rs`, `crates/ava-network/src/throttling/outbound_msg.rs`.
> **Done (2026-06-06, parallel worktree agent):** `MessageQueue` trait (`#[async_trait]`) + `ThrottledMessageQueue` (unbounded `VecDeque` + `parking_lot::Mutex` + `tokio::sync::Notify`) + `BlockingMessageQueue` (bounded `tokio::mpsc`); `OutboundMsgThrottler` (3 byte pools, RAII `OutboundReleasePermit`). 12 new tests. **Findings:** (a) byte length read from `OutboundMessage.bytes.len()`, bypass from `OutboundMessage.bypass_throttling` (both `pub`). (b) **No real dependency on M2.11's `PeerConfig`** — the queue/throttler take their inputs (pool sizes, throttler handle) as constructor params, so M2.12 was implemented in parallel from the Wave-B base. (c) The Go per-node *fairness `waitingToAcquire` queue applies only to the BLOCKING inbound throttler*; the outbound throttler is non-blocking (`acquire(size,node)->bool`, drops on refusal) so needs no waiter queue. (d) **Validator-weight pooling deferred:** `acquire_for(size,node,vdr_bytes)` takes the validator budget explicitly; `acquire(size,node)` defaults it to 0 (→ at-large only). Wiring the validator-set source is M2.14+. (e) `pop` registers the `Notify` waiter and drops the sync mutex *before* awaiting (`17` §7). (f) `bytes` is a dev-only dep (lib reads `Bytes` through `OutboundMessage`).
- [ ] **Step 1 — Red:** `crates/ava-network/tests/message_queue.rs`: `#[tokio::test] async fn throttled_queue_push_pop_fifo()` pushes 3 messages, `pop().await` returns them FIFO. `#[tokio::test] async fn bypass_throttling_skips_acquire()` shows a `bypass_throttling` message is enqueued even when the byte throttler is exhausted, while a normal one returns `push() == false` (dropped, `17` §3 rule 2). `#[tokio::test] async fn drop_releases_throttler_bytes()` asserts the RAII permit returns bytes on pop/drop (no leak).
- [ ] **Step 2 — Confirm red:** `cargo test -p ava-network --test message_queue` → fails.
- [ ] **Step 3 — Green:** implement `trait MessageQueue { fn push(&self, OutboundMessage) -> bool; async fn pop(&self) -> Option<OutboundMessage>; fn pop_now(&self) -> Option<OutboundMessage>; fn close(&self); }`. `ThrottledMessageQueue`: `Mutex<VecDeque>` + `Notify` (no lock across `.await`, `17` §7); push acquires bytes from the outbound byte throttler (per-node + at-large pools, `05` §5) unless `bypass_throttling`; permit released on pop/drop (RAII). `BlockingMessageQueue`: bounded `mpsc`. Outbound throttler returns a non-blocking `acquire(msg,node) -> bool` (drop on refusal).
- [ ] **Step 4 — Confirm green:** `cargo test -p ava-network --test message_queue` passes.
- [ ] **Step 5 — Commit:** `ava-network: throttled/blocking outbound MessageQueue + byte throttler (RAII permits)`.

### Task M2.13: inbound byte throttler + inbound-conn-upgrade throttler ✅ COMPLETED
**Crate:** ava-network  ·  **Depends on:** M2.11  ·  **Spec:** `05` §5, `17` §3
**Files:** `crates/ava-network/src/throttling/inbound_msg_byte.rs`, `crates/ava-network/src/throttling/inbound_conn_upgrade.rs`, `crates/ava-network/src/throttling/mod.rs`.
> **Done (2026-06-06, parallel worktree agent):** `InboundMsgByteThrottler` (3 pools: vdr 32 MiB / at-large 6 MiB / node-max 2 MiB; fair per-node single-outstanding queue; RAII `ReleasePermit`) + `InboundConnUpgradeThrottler` (per-IP cooldown 10s + global `MaxConnsPerSec=256`). 4 new tests. **Findings:** (a) **`acquire` returns `Option<ReleasePermit>`** — `None` on cancel-while-blocked OR when the node already has an outstanding blocked acquire (single-outstanding invariant); partially-reserved bytes are released on cancel. `acquire(size,node,cancel)` delegates to `acquire_with_vdr_alloc(...,vdr_alloc=0)`. (b) **Spec correction — DashMap/governor were NOT used:** the Go reference guards all conn-upgrade state under one mutex + a bounded channel, so a `parking_lot::Mutex<HashMap<IpAddr,Instant>>` + a hand-rolled token bucket is a faithful, dependency-free port. The spec §5 table's "`DashMap<IpAddr,Instant>` + global rate limiter" is one valid design but not what Go does; the single-mutex port is preferred. (c) Conn-upgrade clock injection: `should_upgrade_at(ip, now)` is the testable core, `should_upgrade(ip)` calls it with `Instant::now()`; fully passive (no background `Dispatch`/`Stop` task — expiry computed lazily). (d) Go `release` fairness (oldest-first at-large hand-back + own-node vdr hand-back) ported via a monotonic-id `BTreeMap` (the `linked.Hashmap` analogue). (e) Validator weighting deferred (same `vdr_alloc=0` default as M2.12).
- [ ] **Step 1 — Red:** `crates/ava-network/tests/inbound_throttler.rs`: `#[tokio::test] async fn acquire_blocks_then_releases()` — fill the at-large pool, a second `acquire(size, node, &cancel).await` blocks until the first permit drops, then proceeds (progress guarantee + release-on-drop). `#[tokio::test] async fn per_node_single_outstanding_acquire()` — a node with one outstanding acquire cannot starve others (fairness, `05` §5). `#[test] fn conn_upgrade_cooldown_rejects()` — same IP within the 10s cooldown is rejected.
- [ ] **Step 2 — Confirm red:** `cargo test -p ava-network --test inbound_throttler` → fails.
- [ ] **Step 3 — Green:** implement the inbound msg byte throttler (three pools: vdr 32 MiB, at-large 6 MiB, node-max 2 MiB; `async fn acquire(&self, size, node, &CancellationToken) -> ReleasePermit` with the Go fairness/per-node `waitingToAcquire` queue, woken on release). Implement the inbound-conn-upgrade throttler (`DashMap<IpAddr, Instant>` cooldown + global rate limiter; reject = drop TCP). All permits RAII.
- [ ] **Step 4 — Confirm green:** `cargo test -p ava-network --test inbound_throttler` passes.
- [ ] **Step 5 — Commit:** `ava-network: inbound byte throttler (fair pools) + conn-upgrade throttler`.

### Task M2.14: Peer actor — read/write/net-messages tasks + handshake state ✅ COMPLETED
**Crate:** ava-network  ·  **Depends on:** M2.12, M2.13, M2.9  ·  **Spec:** `05` §1.1/§1.4/§3.2, `17` §2 (#5/#6/#7), §3, §4, §7
**Files:** `crates/ava-network/src/peer/peer.rs`, `crates/ava-network/src/peer/handle.rs`, `crates/ava-network/src/peer/mod.rs`.
- [ ] **Step 1 — Red:** `crates/ava-network/tests/peer_actor.rs`: `#[tokio::test] async fn write_task_sends_handshake_first()` — start a `Peer` over `tokio::io::duplex`; the very first frame the peer writes decodes (via `MsgBuilder::unmarshal`) to `Op::Handshake` (forced first action, `05` §1.4). `#[tokio::test] async fn read_task_resets_deadline_and_drops_oversized()` — feed a length prefix `> 2 MiB` and assert the peer closes (`on_closed` fires). `#[tokio::test] async fn cancel_token_drains_tasks()` — cancelling the peer token joins all three tasks (`17` §4.2).
- [ ] **Step 2 — Confirm red:** `cargo test -p ava-network --test peer_actor` → fails.
- [ ] **Step 3 — Green:** implement `Peer` (`Arc<Peer>` shared by 3 tasks), `PeerHandle { cmd_tx, on_finish_handshake, on_closed }`, `enum PeerCommand { Send, GetPeerList, Close }`. **read task** (`17` #5): `read 4 bytes → read_msg_len → inbound throttler.acquire(len).await → read len into Bytes → parse_inbound → dispatch`; wrap reads in `tokio::time::timeout(PongTimeout=30s)`; network ops handled inline, others only after `finished_handshake` → `router.handle_inbound`; zero-copy `bytes::Bytes` (`17` §10). **write task** (`17` #6): force `Handshake` first, then drain queue with vectored `write_vectored` of `len || payload` (`05` §4.3 / `17` §10). **net-messages task** (`17` #7): `select!` over `getPeerListChan` (cap 1), `ping_interval` (22.5s), `close` token. Last task to drop calls `Network::disconnected` (strong-count drop guard / `TaskTracker`). Hold no sync lock across `.await` (`17` §7); peer/net tokens layered per `17` §4.1.
- [ ] **Step 4 — Confirm green:** `cargo test -p ava-network --test peer_actor` passes.
- [ ] **Step 5 — Commit:** `ava-network: Peer actor (read/write/net tasks) + handshake-first write + drain`.

### Task M2.15: `handle_handshake` (all disconnect reasons) → PeerList reply → connected ✅ COMPLETED
**Crate:** ava-network  ·  **Depends on:** M2.14, M2.11  ·  **Spec:** `05` §1.4, `26` §3.1
**Files:** `crates/ava-network/src/peer/peer.rs` (handshake handling), `crates/ava-network/src/peer/handshake.rs`.
- [ ] **Step 1 — Red:** `crates/ava-network/tests/handshake.rs`: `#[tokio::test] async fn handshake_then_peerlist_completes()` — two in-process `Peer`s over `tokio::io::duplex` exchange `Handshake` then each replies `PeerList`; on receiving `PeerList` while `got_handshake`, `finished_handshake` is set and `ExternalHandler::connected` fires once. Table of disconnect cases (each asserts the connection closes before connected): wrong `network_id`; clock skew `> 60s`; incompatible version (`26` §3); `> 16` tracked subnets; `supported_acps ∩ objected_acps ≠ ∅`; zero port / invalid IP; invalid TLS-IP sig; duplicate Handshake; bloom salt `> 32`.
- [ ] **Step 2 — Confirm red:** `cargo test -p ava-network --test handshake` → fails.
- [ ] **Step 3 — Green:** implement `handle_handshake` per `05` §1.4: validate `network_id`; `|my_time - peer_time| ≤ 60s`; parse `Client` → `AppVersion`, run `version_compatibility.compatible(peer)` (`26` §3.1, close on false); `≤ 16` tracked subnets; verify `SignedIp`; verify BLS PoP if a registered validator; `supported ∩ objected == ∅`; filter ACPs to `CurrentACPs`; reject duplicate; bloom salt `≤ 32`. On success set `got_handshake`, send `PeerList` (`bypass_throttling=true`). `handle_peer_list`: if `!finished_handshake && got_handshake` → `Network::connected(id)`, set `finished_handshake`, notify `on_finish_handshake`, `Track(discovered_ips)`. `GetPeerList` is **not** answered until finished. Add the §1.4 disconnect-reason error variants.
- [ ] **Step 4 — Confirm green:** `cargo test -p ava-network --test handshake` passes.
- [ ] **Step 5 — Commit:** `ava-network: handle_handshake (all disconnect reasons) + PeerList completion`.

### Task M2.16: ping/pong + uptime tracking + `should_disconnect` ✅ COMPLETED
**Crate:** ava-network  ·  **Depends on:** M2.15  ·  **Spec:** `05` §1.5/§3.2, `26` §3.1
**Files:** `crates/ava-network/src/peer/peer.rs` (ping/pong handlers, net-messages tick).
- [ ] **Step 1 — Red:** `crates/ava-network/tests/ping_pong.rs`: `#[tokio::test] async fn ping_carries_uptime_and_pong_records_rtt()` — peer A's net-task ticks, sends `Ping{uptime}`; peer B replies `Pong`; A records RTT (`last_ping_sent` cleared). `#[tokio::test] async fn ping_uptime_over_100_closes()` — a `Ping{uptime=101}` closes the connection. `#[tokio::test] async fn unsolicited_pong_closes()`. `#[tokio::test] async fn should_disconnect_on_clock_crossing_upgrade()` — a peer compatible pre-upgrade is dropped on the next tick after the mock clock crosses `upgrade_time` (`26` §3.1).
- [ ] **Step 2 — Confirm red:** `cargo test -p ava-network --test ping_pong` → fails.
- [ ] **Step 3 — Green:** net-messages tick (`PingFrequency = 22.5s`): re-check `AllowConnection` + `should_disconnect`, send `Ping{uptime}` where `uptime = UptimeCalculator.percent * 100`. On `Ping`: store peer `uptime` (close if `> 100`), reply `Pong`. On `Pong`: RTT = `now - last_ping_sent` (ms), record metric, clear `last_ping_sent`; unsolicited `Pong` closes. `should_disconnect` re-runs `compatible(peer)` and the BLS-PoP check, caching `txid_of_verified_bls_key`. `ObservedUptime()` surfaces what peers think our uptime is.
- [ ] **Step 4 — Confirm green:** `cargo test -p ava-network --test ping_pong` passes.
- [ ] **Step 5 — Commit:** `ava-network: ping/pong + uptime + should_disconnect (compat re-check)`.

### Task M2.17: IP-tracker + PeerList/GetPeerList gossip (bloom + salt) ✅ COMPLETED
**Crate:** ava-network  ·  **Depends on:** M2.16, M0 (ava-types bloom)  ·  **Spec:** `05` §3.5/§3.7, `18` §2.1 (`num_useless_peerlist_bytes`)
**Files:** `crates/ava-network/src/network/ip_tracker.rs`, `crates/ava-network/src/network/tracked_ip.rs`, `crates/ava-network/src/peer/peer.rs` (GetPeerList/PeerList handling).
- [ ] **Step 1 — Red:** `crates/ava-network/tests/ip_gossip.rs`: `#[test] fn peers_excludes_known_via_bloom()` — `ip_tracker.peers(node, tracked, all_subnets, filter, salt)` returns only IPs **not** matched by the bloom filter (so we don't resend known peers). `#[test] fn claimed_ip_port_verified_before_track()` — a `ClaimedIpPort` with a bad signed IP is rejected; a valid one is tracked. `#[test] fn bloom_salt_over_max_rejected()` — salt `> 32` bytes rejected (cross-checks §1.4).
- [ ] **Step 2 — Confirm red:** `cargo test -p ava-network --test ip_gossip` → fails.
- [ ] **Step 3 — Green:** implement `IpTracker` (`05` §3.5): `peers(...)` returns not-yet-known validator IPs using the requester's bloom filter+salt; `ManuallyTrack`/`Track` add to `tracked_ips` (`DashMap<NodeId, TrackedIp>`) with exponential reconnect backoff (1s → 1m). On inbound `PeerList`: for each `ClaimedIpPort`, `staking::parse_certificate`, validate the signed IP, track only if it verifies (`05` §3.5); cadence constants from `networking.go` (gossip 1m, pull 2s, bloom reset 1m, ≤15 validator IPs). Answer `GetPeerList` only after handshake finished. Track `num_useless_peerlist_bytes`. `requestAllSubnetIPs` for primary-network validators (`05` §3.7).
- [ ] **Step 4 — Confirm green:** `cargo test -p ava-network --test ip_gossip` passes.
- [ ] **Step 5 — Commit:** `ava-network: IP-tracker + PeerList/GetPeerList gossip (bloom+salt, verified ClaimedIpPort)`.

### Task M2.18: dialer + accept loop + `Network::dispatch` + runTimers ✅ COMPLETED
**Crate:** ava-network  ·  **Depends on:** M2.17, M2.13  ·  **Spec:** `05` §3.1/§3.4, `17` §2 (#1/#2/#3/#4), §4.3
**Files:** `crates/ava-network/src/dialer.rs`, `crates/ava-network/src/network/mod.rs` (dispatch, accept loop, runTimers), `crates/ava-network/src/network/peer_set.rs`.
- [ ] **Step 1 — Red:** `crates/ava-network/tests/network_dispatch.rs`: `#[tokio::test] async fn two_networks_connect_locally()` — two `NetworkImpl` instances on loopback; A `manually_track`s B; after `dispatch`, A dials B, both upgrade + handshake + reach `connected` (B appears in A's `connected_peers`). `#[tokio::test] async fn start_close_drains_all_tasks()` — `start_close()` closes the listener, cancels `net_token`, every peer actor unwinds, `dispatch` returns (`17` §4.3 step 8).
- [ ] **Step 2 — Confirm red:** `cargo test -p ava-network --test network_dispatch` → fails.
- [ ] **Step 3 — Green:** implement `Dialer::dial(ip).await -> TcpStream` (timeout 30s + token-bucket dial throttle RPS=50, `governor`). Implement `NetworkImpl` holding `peer_config`, `listener`, `dialer`, `server_upgrader`/`client_upgrader`, `ip_tracker`, `connecting_peers`/`connected_peers` (`DashMap`/`arc-swap`, lock-free reads `05` §3.1/§10), throttlers, and a root `CancellationToken`. `async fn dispatch(self: Arc<Self>)`: accept loop (#1) gated by the conn-upgrade throttler (#3) → upgrade → spawn Peer actor with a grandchild `peer_token`; dialer (#2) for tracked IPs with reconnect backoff; `runTimers` (#4): peerlist-pull/bloom-reset/uptime tickers under `select!` on the token. `start_close()` idempotent (close listener, cancel token, `TaskTracker::close().wait()`). Preserve the documented lock order (`peers` before `manually_tracked_ids`, `05` §3.1).
- [ ] **Step 4 — Confirm green:** `cargo test -p ava-network --test network_dispatch` passes.
- [ ] **Step 5 — Commit:** `ava-network: dialer + accept loop + Network::dispatch + runTimers + graceful close`.

### Task M2.19: NAT port mapper (UPnP / NAT-PMP) ✅ COMPLETED
**Crate:** ava-network  ·  **Depends on:** M2.18  ·  **Spec:** `05` §6, `17` §2 (#23)
**Files:** `crates/ava-network/src/nat/mod.rs`, `crates/ava-network/src/nat/port_mapper.rs`.
- [ ] **Step 1 — Red:** `crates/ava-network/tests/nat.rs`: `#[test] fn get_router_falls_back_to_no_router()` — `get_router()` returns a router whose `supports_nat()` is consistent (in CI with no gateway it returns the no-op router). `#[tokio::test] async fn port_mapper_unmaps_on_shutdown()` — a `PortMapper` over a mock `NatRouter` maps the staking port on start and calls `unmap_port` on shutdown.
- [ ] **Step 2 — Confirm red:** `cargo test -p ava-network --test nat` → fails.
- [ ] **Step 3 — Green:** implement `trait NatRouter { fn supports_nat; fn map_port; fn unmap_port; fn external_ip; }`, `get_router() -> Box<dyn NatRouter>` (probe UPnP via `igd-next`, then NAT-PMP/PCP, else `NoRouter`), and a `PortMapper` background task (re-map every `mapTimeout = 30m`, `maxRefreshRetries = 3`, unmap on shutdown via its `CancellationToken`, `17` #23).
- [ ] **Step 4 — Confirm green:** `cargo test -p ava-network --test nat` passes.
- [ ] **Step 5 — Commit:** `ava-network: NAT router (UPnP/NAT-PMP) + PortMapper task`.

### Task M2.20: network metrics + Connect-service enumeration note (R5) ✅ COMPLETED
**Crate:** ava-network  ·  **Depends on:** M2.18  ·  **Spec:** `18` §2.1–§2.3, `05` §3.8 (R5), `15` §3
**Files:** `crates/ava-network/src/metrics.rs`, `crates/ava-network/src/peer/metrics.rs`, `crates/ava-network/tests/metrics.rs`, `crates/ava-network/docs/connect-services.md` (R5 enumeration note).
- [ ] **Step 1 — Red:** `crates/ava-network/tests/metrics.rs`: `#[test] fn metric_names_match_go()` registers the network metrics into a `prometheus::Registry` and asserts the gathered family names include `avalanche_network_peers`, `avalanche_network_times_connected`, `avalanche_network_tls_conn_rejected`, `avalanche_network_num_useless_peerlist_bytes`, the per-peer `avalanche_network_msgs` with labels `io`/`op`/`compressed`, and the throttler gauges from `18` §2.3 (e.g. `avalanche_network_byte_throttler_inbound_remaining_at_large_bytes`).
- [ ] **Step 2 — Confirm red:** `cargo test -p ava-network --test metrics` → fails (metrics not registered).
- [ ] **Step 3 — Green:** register the `avalanche_network_*` families with exact names/labels from `18` §2.1–§2.3 (the `prometheus` crate, `00` §193) and increment them at the peer/throttler call sites (`times_connected`, `tls_conn_rejected`, `msgs{io,op,compressed}`, `msgs_bytes`, `msgs_bytes_saved`, throttler remaining/awaiting gauges). Write `docs/connect-services.md`: record that inter-node p2p uses **no gRPC/Connect services** (R5) and enumerate the deferred services (`proto/` gRPC + `connectproto/` `proposervm`/`xsvm` from `15` §3) as owned by later milestones (rpcchainvm, API), not M2.
- [ ] **Step 4 — Confirm green:** `cargo test -p ava-network --test metrics` passes.
- [ ] **Step 5 — Commit:** `ava-network: avalanche_network_* metrics + Connect-service enumeration note (R5)`.

### Task M2.21: `prop::handshake_reaches_connected` (in-process duplex) ✅ COMPLETED
**Crate:** ava-network  ·  **Depends on:** M2.18  ·  **Spec:** `05` §9 (5), `02` §4
**Files:** `crates/ava-network/tests/prop_handshake.rs`, `crates/ava-network/proptest-regressions/` (committed).
- [ ] **Step 1 — Red:** `prop::handshake_reaches_connected`: `proptest!` over arbitrary-but-valid handshake parameters (network_id pair equal, clocks within 60s, compatible versions, ≤16 subnets, disjoint supported/objected ACPs) — two `Peer`s over `tokio::io::duplex` always reach `finished_handshake` and `connected` fires exactly once on each side; and over a strategy that injects one §1.4 violation, the connection always closes before `connected`.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-network -E 'test(handshake_reaches_connected)'` → fails until wired against the real actor.
- [ ] **Step 3 — Green:** implement the proptest with `FileFailurePersistence::SourceParallel("proptest-regressions")` (`02` §4.1); use a small tokio runtime per case (`tokio::runtime::Runtime` is allowed in tests, `17` §1.1). Add `arbitrary` strategies for handshake params.
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-network -E 'test(handshake_reaches_connected)'` passes; regression corpus committed.
- [ ] **Step 5 — Commit:** `ava-network: prop::handshake_reaches_connected`.

### Task M2.22: `differential::interop_handshake` (live Go Fuji node)
**Crate:** ava-network (integration)  ·  **Depends on:** M2.21, cross-cutting harness X  ·  **Spec:** `05` §9 (9), `26` §9 (4), `02` §9
**Files:** `tests/differential/interop_handshake.rs`, `tests/differential/fixtures/fuji_transcript.bin` (recorded fallback), `tests/differential/proptest-regressions/`, `crates/ava-network/tests/PORTING.md`.
- [ ] **Step 1 — Red:** `differential::interop_handshake`: `#[tokio::test] async fn interop_handshake()` — a Rust node dials a **live Go node on Fuji** (address from env, e.g. `AVA_INTEROP_FUJI_ADDR`), completes the TLS 1.3 handshake, exchanges `Handshake` + receives a `PeerList`, and holds the connection `≥ N` seconds (N from env, default 30) with **no disconnect** and no protocol error. Behind `#[cfg(feature = "interop")]` / `#[ignore]` unless the env gate is set. The **per-PR fallback** path replays `fuji_transcript.bin` (a recorded Go-node transcript) through the codec + handshake state machine and asserts the same outcome offline. Initially the live arm fails (no network stack end-to-end) — the right reason.
- [ ] **Step 2 — Confirm red:** live arm: `AVA_INTEROP_FUJI_ADDR=<peer> cargo nextest run -E 'test(interop_handshake)' --features interop` → fails until M2.7–M2.18 are byte-exact (handshake rejected). Fallback arm: `cargo nextest run -E 'test(interop_handshake)'` (no feature) replays the transcript.
- [ ] **Step 3 — Green:** with the message frames byte-exact (M2.6) and the peer actor complete (M2.18), drive the Rust `NetworkImpl` to dial the Go node; on green, **record** the live transcript into `fuji_transcript.bin` for the offline fallback (coordinate capture with cross-cutting harness X, `02` §9). Gate the live arm behind the `interop` feature + env so CI per-PR runs only the recorded fallback; a scheduled/nightly job runs the live arm (`02` §9, `26` §9.4). Update `PORTING.md` for the `05` §9.9 differential row.
- [ ] **Step 4 — Confirm green:** fallback: `cargo nextest run -E 'test(interop_handshake)'` green every PR; live (gated): green against a real Fuji peer, connection held ≥ N s, no disconnect.
- [ ] **Step 5 — Commit:** `ava-network: differential::interop_handshake (live Fuji + recorded fallback)`.

### Task M2.23: Milestone exit gate
**Crate:** ava-message, ava-network  ·  **Depends on:** ALL prior M2 tasks  ·  **Spec:** all M2 owning specs; `02` §10/§13
**Files:** `crates/ava-message/tests/PORTING.md`, `crates/ava-network/tests/PORTING.md`, `.config/nextest.toml` (ensure `ci` profile covers M2 tests), workspace `Cargo.toml` (members include both crates).
- [ ] **Step 1 — Red:** add a meta `#[test] fn m2_exit_gate_placeholder()` (or a CI checklist item) that fails until every named exit test exists and is green; assert `tests/PORTING.md` has no `wip` rows for the M2 surfaces.
- [ ] **Step 2 — Confirm red:** run the full named set and observe any remaining red: `cargo nextest run --profile ci -E 'test(message_frames) + test(frame_roundtrip) + test(handshake_reaches_connected) + test(interop_handshake)'`.
- [ ] **Step 3 — Green:** ensure all of the following pass (BUILDABLE-&-GREEN INVARIANT):
  - `cargo build --workspace`
  - `cargo build -p avalanchers` (binary still builds; `--version`/`--help` work — smoke run them)
  - `cargo nextest run --profile ci` (whole workspace)
  - `cargo clippy --workspace -- -D warnings`
  - the named exit tests: `golden::message_frames`, `prop::frame_roundtrip`, fuzz `decode_never_overreads` (smoke run), `prop::handshake_reaches_connected`, `differential::interop_handshake` (recorded-fallback in CI; live arm behind the `interop` feature/env, per M2.22).
  - update both crates' `tests/PORTING.md` matrices (no `wip` rows for the `05`/`15 §3`/`26` handshake surfaces).
- [ ] **Step 4 — Confirm green:** the full invariant command block above is green; `./target/debug/avalanchers --version` and `--help` succeed.
- [ ] **Step 5 — Commit:** `M2: networking handshake exit gate green (build + nextest + clippy + golden/prop/fuzz/differential)`.

---

## Spec coverage check

| Spec section | Subject | Task(s) |
|---|---|---|
| `05` §0 | Go source map | informational (all tasks) |
| `05` §1.1 | Transport & framing (4-byte BE len, 2 MiB cap, vectored write, deadlines) | M2.3, M2.14 |
| `05` §1.2 | Op-code table + UNREQUESTED_OPS / FAILED_TO_RESPONSE_OPS | M2.2 |
| `05` §1.3 | zstd recursive packing (R4 decode-only) + per-op compression decision | M2.4, M2.5, M2.6 |
| `05` §1.4 | Handshake sequence + all disconnect reasons | M2.15 |
| `05` §1.5 | Ping/Pong + uptime tracking | M2.16 |
| `05` §1.6 | TLS & identity (TLS1.3, mutual, leaf-key policy, NodeID-from-cert, IP signing) | M2.7, M2.8, M2.9, M2.10 |
| `05` §2.1 | Generated proto module | M2.1 |
| `05` §2.2 | Op enum + Message wrapper | M2.2 |
| `05` §2.3 | MsgBuilder codec + frame helpers + OnFinished RAII | M2.3, M2.4 |
| `05` §2.4 | Builder API (OutboundMsgBuilder) | M2.5 |
| `05` §3.1 | Network trait & service + lock discipline | M2.11, M2.18 |
| `05` §3.2 | Peer actor (3 tasks) + should_disconnect | M2.14, M2.16 |
| `05` §3.3 | Outbound message queue (throttled/blocking) | M2.12 |
| `05` §3.4 | Dialer | M2.18 |
| `05` §3.5 | Peer-list gossip, IP tracking, IP signer | M2.10, M2.17 |
| `05` §3.6 | InboundHandler / ExternalHandler handoff traits | M2.11 |
| `05` §3.7 | Tracked subnets | M2.15, M2.17 |
| `05` §3.8 | App-level SDK (AppRequest/Gossip, uvarint, odd request-ids), gossip, acp118 | **Deferred** — later milestone (app/VM SDK); R5 note recorded in M2.20 |
| `05` §4 | TLS layer + custom verifiers + upgrader | M2.7, M2.8, M2.9 |
| `05` §5 | Throttling (inbound/outbound byte, conn-upgrade) | M2.12, M2.13; bandwidth/buffer/resource throttlers **partially deferred** (only the handshake-path ones needed now — full set in a later networking milestone) |
| `05` §6 | NAT (UPnP / NAT-PMP) | M2.19 |
| `05` §7 | Config knobs (NetworkConfig defaults) | **Deferred to ava-config milestone** — constants cited inline in M2.13/M2.16/M2.18; full `network-*` flag surface owned by `12`/`13` |
| `05` §8 | Go→Rust mapping | M2.14 (actor), M2.12 (queue), M2.8/§4 (verifiers) |
| `05` §9 | Test plan (golden, proptest, mock-peer, TLS interop, throttler, differential) | M2.6, M2.9, M2.10, M2.13, M2.21, M2.22 |
| `05` §10 | Perf notes (zero-copy, vectored writes, RAII, lock-free) | applied in M2.4, M2.12, M2.14, M2.18 |
| `15` §1.1 / §4.2 | p2p framing byte-exactness | M2.3, M2.6 |
| `15` §3.1 | p2p oneof tags + sub-message fields | M2.1, M2.5 |
| `15` §4.2 | zstd decode-compatible (R4) | M2.4, M2.6 |
| `15` §5 | prost mapping (`bytes::Bytes`, oneof) | M2.1 |
| `15` §3.2–§3.18 | other gRPC/Connect services | **Deferred** — enumerated in M2.20 (R5), owned by rpcchainvm/API milestones |
| `17` §1 | One runtime, no sub-runtimes (tests excepted) | M2.14, M2.18 (spawn onto ambient runtime) |
| `17` §2 | Task graph (#1 accept, #2 dialer, #3 conn-upgrade, #4 runTimers, #5/#6/#7 peer, #23 NAT) | M2.14, M2.18, M2.19 |
| `17` §3 | Channel sizing & backpressure (outbound drop, getPeerListChan cap 1, inbound throttle never-drop) | M2.12, M2.13, M2.14 |
| `17` §4 | Cancellation tree + graceful drain (net_token/peer_token, start_close) | M2.14, M2.18 |
| `17` §7 | Send/Sync, actor pattern, no-lock-across-await | M2.12, M2.14 |
| `26` §2 | Version string the port reports (`avalanchego/1.14.2`) | M2.5 (Client fields), M2.11 |
| `26` §3 | Compatibility checker / min-compatible-peer rule | M2.11, M2.15, M2.16 |
| `18` §2.1 | `avalanche_network_*` core metrics | M2.20 |
| `18` §2.2 | per-peer message I/O metrics (`msgs{io,op,compressed}`) | M2.20 |
| `18` §2.3 | throttling metrics | M2.20 |

**Deferrals (explicit):** (1) app-level SDK `ava-network::p2p` (AppRequest/Response/Gossip framing, uvarint handler prefix, odd request-ids, gossip set-reconciliation, acp118) — `05` §3.8, deferred to the VM/app milestone. (2) Full throttler set beyond the handshake path (bandwidth, buffer, inbound resource/CPU-disk throttlers) — `05` §5, partial in M2.13. (3) The `NetworkConfig` flag surface — `05` §7, owned by `12`/`13` (constants cited inline now). (4) All non-p2p gRPC/Connect services — `15` §3.2–§3.18, enumerated under R5 in M2.20 and owned by later milestones. (5) Consensus/state-sync/bootstrap/app op builders beyond handshake-class — stubbed in M2.5, filled when their consuming engine milestones land. (6) **Live metric increments** — M2.20 registers the full `avalanche_network_*` family set (the parity surface) but leaves the `+1` call sites as documented `// metrics:` notes to avoid refactoring the M2.13/M2.14 constructors; wiring them is folded into M2.23. The node-level `avalanche_network_` prefix is applied by `ava-api`'s `PrefixGatherer` (spec `18` §1.1/§3), deferred to the node milestone.
