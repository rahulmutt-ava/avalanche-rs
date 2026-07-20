# C-Chain Tx Gossip Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Coreth-parity C-Chain transaction gossip (push + pull) over a new `ava-p2p` SDK crate, proven live in the mixed Go+Rust network, with the `rust_proposes` proposer detection reworked to survive gossip.

**Architecture:** New crate `ava-p2p` ports Go `network/p2p` + `network/p2p/gossip` atop the existing `ava-vm` `AppHandler`/`AppSender`/`Connector` traits. The engine gains inbound `App*` + `Connected`/`Disconnected` routing (`InboundOp` variants → adapters → VM). `EvmVm::initialize` constructs the p2p network from its already-provided `AppSender` (coreth `vm.go` shape); boot only swaps `NoopAppSender` for a real adapter over the chain's `OutboundSender`.

**Tech Stack:** Rust workspace (cargo/nextest via `./scripts/nix_run.sh`), prost (message-only, no tonic services), tokio, existing byte-exact `utils/bloom` port.

**Design spec:** `docs/superpowers/specs/2026-07-20-cchain-tx-gossip-design.md`
**Go oracle:** `~/avalanchego` — cite exact files/lines; run `./scripts/check_oracle_binary.sh` before any oracle capture or live run.

## Global Constraints

- License header on every new `.rs` file: `// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.` + `// See the file LICENSE for licensing terms.`
- No `unwrap()`/`expect()`/`todo!` in library code; `thiserror` per-crate error enums; `#![forbid(unsafe_code)]`.
- Imports `StdExternalCrate`; run `./scripts/nix_run.sh cargo fmt` before every commit.
- All cargo commands via `./scripts/nix_run.sh cargo ...`; foreground only, generous timeouts (600000 ms); never background a cargo command.
- Root `Cargo.toml` `[workspace] members` is an **explicit list** — new crates must be added by name (rules_rust cold-splice drops globs).
- Gossip loops: injected/paused-time-testable timing, `CancellationToken`-tied; no `SystemTime::now()` on consensus paths (lint-determinism). Bloom salt randomness reuses the existing `fill_random_pub` (already in the bloom module); add an `xtask/determinism-allowlist.toml` entry only if the lint flags a new site.
- Every task ends: scoped `cargo nextest run -p <touched crates>` green + `cargo clippy -p <touched> --all-targets -- -D warnings` clean + fmt clean + commit.
- Tests: `assert_matches!`/`pretty_assertions`; no `require`-style helpers; name tests so `-E 'test(name)'` matches the fn name.

---

### Task 1: Hoist `utils/bloom` to `ava-utils`

**Files:**
- Create: `crates/ava-utils/src/bloom.rs` (moved content)
- Modify: `crates/ava-utils/src/lib.rs` (add `pub mod bloom;`)
- Modify: `crates/ava-network/src/network/bloom.rs` (becomes a re-export shim)
- Modify: `crates/ava-network/Cargo.toml` (add `ava-utils` dep if absent)

**Interfaces:**
- Produces: `ava_utils::bloom::{Filter, ReadFilter, BloomError, hash, fill_random_pub}` — identical signatures to today's `ava_network::network::bloom` (which keeps working via `pub use ava_utils::bloom::*;`).

- [ ] **Step 1:** Move the file: copy `crates/ava-network/src/network/bloom.rs` (including its `#[cfg(test)]` module) to `crates/ava-utils/src/bloom.rs` verbatim; update the module doc's "ported here (M2.17)…" note to record the hoist. Add `pub mod bloom;` to `ava-utils/src/lib.rs`.
- [ ] **Step 2:** Replace `crates/ava-network/src/network/bloom.rs` body with:

```rust
// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Re-export of the hoisted `utils/bloom` port (now `ava_utils::bloom`; the
//! M2.17 note anticipated this move). All `ava-network` callers keep the
//! `crate::network::bloom::` path.

pub use ava_utils::bloom::*;
```

- [ ] **Step 3:** Add `ava-utils` to `ava-network/Cargo.toml` `[dependencies]` (workspace dep form used by sibling crates) if not already present (it likely is — check first).
- [ ] **Step 4:** Run: `./scripts/nix_run.sh cargo nextest run -p ava-utils -p ava-network` — expect all green (the moved unit tests now run in ava-utils). `cargo clippy -p ava-utils -p ava-network --all-targets -- -D warnings` clean.
- [ ] **Step 5:** Commit: `refactor(ava-utils): hoist utils/bloom port from ava-network (reserved M2.17 refactor)`

---

### Task 2: `ava-p2p` crate scaffold + `proto/sdk` messages

**Files:**
- Create: `crates/ava-p2p/Cargo.toml`, `crates/ava-p2p/build.rs`, `crates/ava-p2p/proto/sdk/sdk.proto`, `crates/ava-p2p/src/lib.rs`, `crates/ava-p2p/src/pb.rs`, `crates/ava-p2p/src/error.rs`
- Modify: root `Cargo.toml` (`members` += `"crates/ava-p2p"`, alphabetical position)

**Interfaces:**
- Produces: `ava_p2p::pb::sdk::{PushGossip, PullGossipRequest, PullGossipResponse}` (prost messages); `ava_p2p::error::{Error, Result}`.

- [ ] **Step 1:** Copy `~/avalanchego/proto/sdk/sdk.proto` verbatim into `crates/ava-p2p/proto/sdk/sdk.proto` (keep the ACP-118 `SignatureRequest`/`SignatureResponse` messages for file fidelity; unused is fine). Field numbers matter: `PullGossipRequest{ salt = 2, filter = 3 }` (field 1 is reserved-by-omission upstream).
- [ ] **Step 2:** Write `build.rs` on the `ava-message` pattern (prost-build, message-only — no tonic services):

```rust
// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    prost_build::Config::new()
        .bytes(["."])
        .compile_protos(&["proto/sdk/sdk.proto"], &["proto"])?;
    println!("cargo:rerun-if-changed=proto/sdk/sdk.proto");
    Ok(())
}
```

Mirror the exact prost-build invocation style from `crates/ava-message/build.rs` (check whether it uses `.bytes(["."])`; match it). `src/pb.rs`:

```rust
// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Generated `proto/sdk` messages (Go `proto/pb/sdk`).

/// `sdk` package messages.
pub mod sdk {
    #![allow(missing_docs, clippy::pedantic)]
    include!(concat!(env!("OUT_DIR"), "/sdk.rs"));
}
```

- [ ] **Step 3:** `Cargo.toml`: workspace-inherited `prost`, `prost-build` (build-dep), `thiserror`, `ava-types`, `ava-utils`, `ava-vm`, `tokio` (`sync`,`time`,`rt`), `tokio-util`, `async-trait`, `tracing`; `[lints] workspace = true` if the root table permits (check a sibling like `ava-message`; copy its form). `src/lib.rs` skeleton: license header, crate doc ("Port of Go `network/p2p` + `network/p2p/gossip`"), `#![forbid(unsafe_code)] #![warn(missing_docs)]`, `pub mod error; pub mod pb;`. `error.rs`: thiserror enum with `Decode(String)`, `Send(String)`, `Set(String)` variants + `pub type Result<T> = std::result::Result<T, Error>;`.
- [ ] **Step 4:** Add the failing round-trip test at the bottom of `pb.rs`:

```rust
#[cfg(test)]
mod tests {
    use prost::Message;

    use super::sdk;

    /// Proto3 wire bytes computed by hand: field 2 (salt, bytes) = tag 0x12,
    /// field 3 (filter, bytes) = tag 0x1a.
    #[test]
    fn pull_gossip_request_wire_bytes_pinned() {
        let req = sdk::PullGossipRequest {
            salt: bytes::Bytes::from_static(&[0xAA, 0xBB]),
            filter: bytes::Bytes::from_static(&[0x01]),
        };
        let enc = req.encode_to_vec();
        assert_eq!(enc, vec![0x12, 0x02, 0xAA, 0xBB, 0x1A, 0x01, 0x01]);
        let dec = sdk::PullGossipRequest::decode(enc.as_slice()).unwrap();
        assert_eq!(dec, req);
    }
}
```

(Adjust `bytes::Bytes` vs `Vec<u8>` to whatever the prost config generates — match `ava-message`'s generated field types.)
- [ ] **Step 5:** Run: `./scripts/nix_run.sh cargo nextest run -p ava-p2p` — expect the test to pass once the crate compiles (RED first = crate doesn't exist / members missing; confirm the members error, then add). Clippy + fmt clean.
- [ ] **Step 6:** Commit: `feat(ava-p2p): crate scaffold + proto/sdk messages (PushGossip, PullGossip*)`

---

### Task 3: Handler trait, varint protocol prefix, `P2pNetwork` mux

**Files:**
- Create: `crates/ava-p2p/src/handler.rs`, `crates/ava-p2p/src/network.rs`
- Modify: `crates/ava-p2p/src/lib.rs` (register modules + re-exports)
- Test: inline `#[cfg(test)]` in both files

**Interfaces:**
- Consumes: `ava_vm::{AppHandler, AppSender, AppError, Connector, SendConfig}`; `ava_types::node_id::NodeId`.
- Produces:
  - `ava_p2p::handler::{Handler, TX_GOSSIP_HANDLER_ID: u64 = 0, ATOMIC_TX_GOSSIP_HANDLER_ID: u64 = 1}`
  - `ava_p2p::handler` error constructors: `err_unexpected() -> AppError` (code −1), `err_unregistered_handler()` (−2), `err_not_validator()` (−3), `err_throttled()` (−4) — Go `network/p2p/error.go`.
  - `ava_p2p::network::{P2pNetwork, protocol_prefix(handler_id: u64) -> Vec<u8>, parse_prefix(msg: &[u8]) -> Option<(u64, &[u8])>}`
  - `P2pNetwork::new(node_id: NodeId, sender: Arc<dyn AppSender>) -> Arc<P2pNetwork>`
  - `P2pNetwork::add_handler(&self, handler_id: u64, handler: Arc<dyn Handler>) -> Client` (Client lands Task 4; in this task return `()` and change in Task 4)
  - `P2pNetwork::sample_peer(&self) -> Option<NodeId>` (uniform over connected peers; pull gossip's sampler)
  - `impl AppHandler for P2pNetwork` + `impl Connector for P2pNetwork` — **note:** those traits take `&mut self`; `P2pNetwork` keeps interior mutability (`parking_lot::Mutex` maps) so the impls are thin.

- [ ] **Step 1:** Write `handler.rs`:

```rust
/// A per-protocol application handler (Go `network/p2p/handler.go` `Handler`).
#[async_trait]
pub trait Handler: Send + Sync {
    /// Handle a gossip payload (prefix already stripped). Errors are dropped
    /// by the caller (gossip is fire-and-forget).
    async fn app_gossip(&self, node: NodeId, msg: &[u8]);
    /// Handle a request payload; `Ok` bytes become the `AppResponse`,
    /// `Err(AppError)` becomes the `AppError` reply.
    async fn app_request(
        &self,
        node: NodeId,
        deadline: Instant,
        msg: &[u8],
    ) -> Result<Vec<u8>, AppError>;
}

/// Go `network/p2p/handler.go:25-29` iota.
pub const TX_GOSSIP_HANDLER_ID: u64 = 0;
/// Atomic tx gossip (out of scope this milestone; reserved).
pub const ATOMIC_TX_GOSSIP_HANDLER_ID: u64 = 1;
```

plus the four `AppError` constructors with the Go codes/messages verbatim (`unexpected error`, `unregistered handler`, `not a validator`, `throttled`).
- [ ] **Step 2:** Failing tests for the prefix in `network.rs` (Go `binary.AppendUvarint` = LEB128; use `prost::encoding::{encode_varint, decode_varint}`):

```rust
#[test]
fn protocol_prefix_matches_go_append_uvarint() {
    assert_eq!(protocol_prefix(0), vec![0x00]);
    assert_eq!(protocol_prefix(1), vec![0x01]);
    assert_eq!(protocol_prefix(127), vec![0x7f]);
    assert_eq!(protocol_prefix(128), vec![0x80, 0x01]);
}

#[test]
fn parse_prefix_splits_handler_id_and_payload() {
    let mut framed = protocol_prefix(1);
    framed.extend_from_slice(b"payload");
    let (id, rest) = parse_prefix(&framed).unwrap();
    assert_eq!(id, 1);
    assert_eq!(rest, b"payload");
    assert!(parse_prefix(&[]).is_none());
}
```

- [ ] **Step 3:** Run `./scripts/nix_run.sh cargo nextest run -p ava-p2p -E 'test(protocol_prefix_matches_go_append_uvarint)'` — FAIL (fn absent).
- [ ] **Step 4:** Implement `network.rs`: `protocol_prefix`/`parse_prefix`; `P2pNetwork { node_id, sender, handlers: parking_lot::Mutex<HashMap<u64, Arc<dyn Handler>>>, peers: parking_lot::Mutex<BTreeSet<NodeId>>, pending: … (Task 4) }`. `AppHandler` impl (mirror Go `network/p2p/router.go:261` dispatch):
  - `app_gossip`: `parse_prefix` → unknown/absent handler → `tracing::debug!` + drop; else `handler.app_gossip(node, rest)`.
  - `app_request`: `parse_prefix` fails or unregistered → `sender.send_app_error(token, node, request_id, -2, "unregistered handler")`; else `handler.app_request(...)` → `Ok(bytes)` ⇒ `send_app_response`, `Err(e)` ⇒ `send_app_error(code, message)`.
  - `app_response`/`app_request_failed`: no-op stubs until Task 4 wires the pending map.
  - `Connector` impl: insert/remove from `peers`; `sample_peer` picks uniformly (index via a small LCG seeded per-call from an `AtomicU64` counter — deterministic, no RNG crate; note the divergence from Go's sampler in a comment).
- [ ] **Step 5:** Add mux tests: register a recording `Handler`; drive `app_gossip`/`app_request` through the `AppHandler` impl with a mock `AppSender` that records `send_app_response`/`send_app_error` calls; assert dispatch, unknown-id error reply (code −2), and gossip-drop.
- [ ] **Step 6:** `./scripts/nix_run.sh cargo nextest run -p ava-p2p` all green; clippy/fmt. Commit: `feat(ava-p2p): Handler trait + varint protocol mux (network/p2p parity)`

---

### Task 4: `Client` — request correlation

**Files:**
- Create: `crates/ava-p2p/src/client.rs`
- Modify: `crates/ava-p2p/src/network.rs` (pending map + `add_handler` returns `Client`), `crates/ava-p2p/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub type OnResponse = Box<dyn FnOnce(NodeId, Result<Vec<u8>, AppError>) + Send>`
  - `Client::app_request(&self, token, node: NodeId, bytes: Vec<u8>, on_response: OnResponse) -> ava_p2p::Result<()>` — allocates `request_id` from a shared `AtomicU32`, inserts into the network's pending map, prefixes, `sender.send_app_request`.
  - `Client::app_gossip(&self, token, cfg: SendConfig, bytes: Vec<u8>) -> ava_p2p::Result<()>` — prefixes + `sender.send_app_gossip`.
  - `P2pNetwork::app_response`/`app_request_failed` now remove the pending entry and invoke the callback exactly once; a response/failure for an unknown id is dropped silently (the router's timeout synthesis can race a real reply — dedup by removal, same safety the engine router relies on).

- [ ] **Step 1:** Failing test: mock `AppSender` records `send_app_request(nodes, id, bytes)` and lets the test feed `app_response(node, id, bytes)` / `app_request_failed(node, id, err)` back into the `P2pNetwork`; assert (a) callback fires with `Ok` payload once, (b) failure path fires `Err`, (c) a second delivery for the same id is a no-op, (d) request bytes on the wire carry the varint prefix.
- [ ] **Step 2:** Run: `-E 'test(client_correlates_response)'` — FAIL. Implement per Interfaces. Run all `-p ava-p2p` green.
- [ ] **Step 3:** Commit: `feat(ava-p2p): Client request/response correlation (network/p2p/client.go parity)`

---

### Task 5: Gossip framework — traits + `BloomSet`

**Files:**
- Create: `crates/ava-p2p/src/gossip/mod.rs`, `crates/ava-p2p/src/gossip/bloom.rs`
- Modify: `crates/ava-p2p/src/lib.rs`; `crates/ava-utils/src/bloom.rs` **only if** `optimal_parameters`/`estimate_count` are missing (port them from Go `utils/bloom/*.go` — check first; the peer-list gossip may not have needed them)

**Interfaces:**
- Produces (in `ava_p2p::gossip`):
  - `pub trait Gossipable: Send + Sync { fn gossip_id(&self) -> Id; }`
  - `pub trait Marshaller<T>: Send + Sync { fn marshal(&self, t: &T) -> Result<Vec<u8>>; fn unmarshal(&self, bytes: &[u8]) -> Result<T>; }`
  - `pub trait Set<T: Gossipable>: Send + Sync { fn add(&self, t: T) -> Result<()>; fn has(&self, id: &Id) -> bool; fn iterate(&self, f: &mut dyn FnMut(&T) -> bool); fn get_filter(&self) -> (Vec<u8>, Vec<u8>); }` — `(bloom_marshal_bytes, salt)`, Go `gossip/set.go`.
  - `BloomSet::new(min_target_elements: usize, target_fpp: f64, reset_fpp: f64) -> Result<BloomSet>`; `add(&mut self, id: &Id)`; `has(&self, id: &Id) -> bool`; `marshal(&self) -> (Vec<u8>, Vec<u8>)`; `reset_if_needed(&mut self, count_hint: usize, refill: &mut dyn FnMut(&mut dyn FnMut(&Id)))` — Go `gossip/bloom.go` `NewBloomFilter`/`ResetBloomFilterIfNeeded`. Wire format of the filter = the existing `ava_utils::bloom::Filter` layout (`num_hashes || seeds || entries`), which is what Go peers parse.
  - **Salt:** 32 random bytes via `ava_utils::bloom::fill_random_pub`; hash of an id = `ava_utils::bloom::hash(id.as_ref(), &salt)`.

- [ ] **Step 1:** Read `~/avalanchego/network/p2p/gossip/bloom.go` and `utils/bloom/{optimal_*.go}` end-to-end before writing code; port `optimal_parameters(count, fpp) -> (num_hashes, num_entries)` and `estimate_count(num_hashes, filter) -> usize` into `ava-utils/src/bloom.rs` if absent, with the Go formulas verbatim (float math is fine here — the bloom is a network filter, not a consensus value; note this in the module doc against the "no floats in consensus paths" rule).
- [ ] **Step 2:** Failing tests: (a) `bloom_set_membership` — add 3 ids, `has` true for them / false for a 4th; (b) `bloom_set_marshal_readable_by_read_filter` — `ReadFilter::parse(marshal().0)` + `contains_key(id, salt)` true; (c) `reset_regenerates_salt_and_refills` — force reset via a tiny `min_target_elements`, assert salt changed and refilled ids still `has`.
- [ ] **Step 3:** Run (FAIL) → implement → run `-p ava-p2p -p ava-utils` green.
- [ ] **Step 4:** Commit: `feat(ava-p2p): gossip traits + BloomSet over the hoisted utils/bloom (gossip/bloom.go parity)`

---

### Task 6: Push gossiper, pull gossiper, pull handler

**Files:**
- Create: `crates/ava-p2p/src/gossip/push.rs`, `crates/ava-p2p/src/gossip/pull.rs`, `crates/ava-p2p/src/gossip/handler.rs`
- Modify: `crates/ava-p2p/src/gossip/mod.rs`
- Test: `crates/ava-p2p/tests/gossip_loops.rs`

**Interfaces:**
- Consumes: `Client` (Task 4), `Set`/`Marshaller`/`Gossipable` (Task 5), `pb::sdk` (Task 2), `P2pNetwork::sample_peer` (Task 3).
- Produces:
  - `GossipParams { target_message_size: usize /* 20 KiB */, push_period: Duration /* 100ms */, pull_period: Duration /* 1s */, regossip_period: Duration /* 30s */, push_cfg: SendConfig /* validators: 100 */, regossip_cfg: SendConfig /* validators: 10 */, discarded_cache_size: usize /* 16_384 */ }` with `Default` = Go `gossip/system.go` `SystemConfig.setDefaults()` + coreth `config/default_config.go:55-61`.
  - `PushGossiper<T, M, S>::new(marshaller: M, set: Arc<S>, client: Client, params: GossipParams)`; `add(&self, t: T)` (queue for next cycle); `gossip_cycle(&self, token) -> Result<()>` (drain new-queue → `PushGossip{gossip}` batches ≤ target size → `client.app_gossip(push_cfg)`; move gossiped to regossip queue; every `regossip_period` re-send regossip queue with `regossip_cfg`; drop ids the set no longer `has` — they were mined/evicted).
  - `PullGossiper<T, M, S>::new(marshaller, set, client, network: Arc<P2pNetwork>, params)`; `pull_cycle(&self, token) -> Result<()>` (`set.get_filter()` → `PullGossipRequest{salt, filter}` → `client.app_request` to one `sample_peer()`; the `OnResponse` decodes `PullGossipResponse` and `set.add`s each, per-tx errors logged and skipped).
  - `GossipHandler<T, M, S>::new(marshaller, set, push: Option<Arc<PushGossiper<T, M, S>>>, params)` implementing `handler::Handler`: `app_gossip` = decode `PushGossip` → per-item unmarshal + `set.add` → on success also `push.add` (transitive forwarding, Go `gossip/handler.go` + `system.go` wiring); `app_request` = decode `PullGossipRequest` → `ReadFilter::parse` → iterate set collecting items whose hashed id is NOT in the requester's filter, stop at `target_message_size` → `PullGossipResponse`. Malformed request → `Err(err_unexpected())`.
  - `pub async fn every<F>(token: CancellationToken, period: Duration, mut cycle: F)` — the `gossip.Every` loop helper (tokio interval, exits on cancel).

- [ ] **Step 1:** Before coding, read `~/avalanchego/network/p2p/gossip/{gossip.go,handler.go,system.go}` end-to-end; where this plan simplifies (single push branch-factor via `SendConfig` instead of stake-percentage sampling — our `OutboundSender` resolves `SendConfig` sampling), record the deviation in the module doc with the Go line cites.
- [ ] **Step 2:** Failing tests in `tests/gossip_loops.rs` using `#[tokio::test(start_paused = true)]`, a `TestItem(Id)` gossipable, an in-memory `HashSet`-backed test `Set`, and the Task-3/4 mock `AppSender`:
  - `push_cycle_emits_prefixed_push_gossip` — `add` two items, run one cycle, decode the recorded gossip bytes: prefix 0x00 + `PushGossip` containing both marshaled items.
  - `pull_cycle_requests_with_current_filter` — recorded `AppRequest` decodes to `PullGossipRequest` whose `(filter, salt)` matches `set.get_filter()`.
  - `pull_response_admits_items` — feed a `PullGossipResponse` back through the client callback; new items land in the set; a corrupt item is skipped, rest admitted.
  - `handler_answers_pull_excluding_known` — requester filter containing item A; handler returns only item B.
  - `handler_admits_pushed_and_forwards` — `app_gossip` with a `PushGossip` → set gains the item and the push gossiper's queue gained it (assert via next `gossip_cycle` output).
- [ ] **Step 3:** Run (FAIL) → implement → `-p ava-p2p` all green. Clippy/fmt.
- [ ] **Step 4:** Commit: `feat(ava-p2p): push/pull gossipers + pull handler (network/p2p/gossip parity)`

---

### Task 7: Engine inbound App routing

**Files:**
- Modify: `crates/ava-engine/src/networking/router.rs` (new `InboundOp` variants; extend `AppRequestFailed`)
- Modify: `crates/ava-engine/src/networking/engine_adapter.rs` (both adapters forward App ops to the VM)
- Modify: `crates/ava-chains/src/create_chain.rs` (pass the shared VM `Arc` into both adapters)
- Modify: `crates/ava-node/src/init/inbound_decode.rs` (decode `AppRequest`/`AppResponse`/`AppGossip`/`AppError`)
- Tests: existing inline test modules in each file + `crates/ava-node/src/init/inbound_decode.rs` tests

**Interfaces:**
- Produces (`InboundOp` additions; existing variants untouched):

```rust
/// `AppRequest` — VM-defined request; `deadline_nanos` is the wire-relative deadline.
AppRequest { request_id: u32, deadline_nanos: u64, bytes: Vec<u8> },
/// `AppResponse` to an `AppRequest` we issued.
AppResponse { request_id: u32, bytes: Vec<u8> },
/// `AppGossip` — VM-defined gossip (no request id).
AppGossip { bytes: Vec<u8> },
```

  and `AppRequestFailed` gains `code: i32, message: String` fields (timeout synthesis in `InboundOp::failed` fills the framework timeout code — use `ava_vm::AppError::TIMEOUT`'s code; check its constant in `ava-vm/src/app.rs` and cite it).
- Adapters: `BootstrapperEngineAdapter::new` and `SnowmanEngineAdapter::new` each gain a `vm: Arc<tokio::sync::Mutex<V>>` parameter (the same typed `Arc` the `Getter` shares — see `create_chain.rs:738` comment); the four App arms call `vm.lock().await.app_request(token, node, id, deadline, &bytes)` etc. `deadline` = `Instant::now() + Duration::from_nanos(deadline_nanos)` (monotonic `Instant`, not wall clock — determinism-lint-safe; confirm).
- Decode (`inbound_decode.rs`): move `M::AppRequest | M::AppResponse | M::AppGossip | M::AppError` out of the ignore arm; `M::AppError(m)` → `InboundOp::AppRequestFailed { request_id: m.request_id, code: m.error_code, message: m.error_message }` (proto field names per `crates/ava-message/proto/p2p/p2p.proto:383-425`).

- [ ] **Step 1:** Failing decode tests (follow the existing test pattern in `inbound_decode.rs`): build each of the four wire messages via `MsgBuilder`/proto structs, assert the produced `(chain, InboundOp)`.
- [ ] **Step 2:** Failing adapter test (pattern: existing `engine_adapter` tests): construct a `SnowmanEngineAdapter` over `ava_vm::testutil::TestVm` (its `AppHandler` impl records calls — `testutil.rs:273-304`), `handle(node, InboundOp::AppGossip{..})`, assert the TestVm observer recorded `app_gossip(node, bytes)`. Same for `AppRequest` (recorded with deadline), `AppResponse`, `AppRequestFailed`, and the Bootstrapper adapter.
- [ ] **Step 3:** Run scoped (FAIL) → implement variants, decode, adapter arms + `create_chain.rs` plumbing of the vm `Arc` (both `BootstrapperEngineAdapter::new`/`SnowmanEngineAdapter::new` call sites) → green: `./scripts/nix_run.sh cargo nextest run -p ava-engine -p ava-node -p ava-chains`.
- [ ] **Step 4:** Check every other `InboundOp` match site compiles (`grep -rn "InboundOp::" crates/`) — the new variants must not fall into `_ => {}` arms where App handling is required; the two adapters are the only required consumers.
- [ ] **Step 5:** Commit: `feat(ava-engine): inbound App* ops routed adapter→VM (AppGossip/AppRequest/AppResponse/AppError)`

---

### Task 8: `Connected`/`Disconnected` plumb to the VM

**Files:**
- Modify: `crates/ava-engine/src/networking/router.rs` (`InboundOp::{Connected, Disconnected}` + `ChainRouter::{connected, disconnected}` broadcasting to all chain sinks — Go `chain_router.go` `Connected`)
- Modify: `crates/ava-engine/src/networking/engine_adapter.rs` (forward to `vm.connected/disconnected`)
- Modify: `crates/ava-node/src/init/networking.rs` (the production `ExternalHandler`/`RouterBridge` `connected`/`disconnected` — today they feed only the `BeaconManager` — also call `chain_router.connected(node, version)`)

**Interfaces:**
- Produces: `InboundOp::Connected { version: ava_version::application::Application }`, `InboundOp::Disconnected`; `Router` trait gains `fn connected(&self, node: NodeId, version: Application)` / `fn disconnected(&self, node: NodeId)` with default no-op impls so existing test `Router` impls keep compiling (check `ava-chains/tests/pipeline.rs`'s impl).
- Note: `ava-engine` may need an `ava-version` dep — check `Cargo.toml` first (it likely has it transitively via ava-vm; add explicitly if the compiler asks).

- [ ] **Step 1:** Failing test: `ChainRouter` with two registered recording sinks; `router.connected(node, v)` → both sinks got `InboundOp::Connected`; `disconnected` likewise. Adapter test: TestVm observer records `connected(node, version)`.
- [ ] **Step 2:** Implement; wire the ava-node `ExternalHandler` impls (all three `fn connected` sites at `networking.rs:97/140/228` — read each; only the production `RouterBridge` one needs the router call, the others are test/beacon-only shims — decide per the surrounding struct and document).
- [ ] **Step 3:** `./scripts/nix_run.sh cargo nextest run -p ava-engine -p ava-node` green. Commit: `feat(ava-engine): peer connect/disconnect delivered to VMs via ChainRouter (chain_router.go parity)`

---

### Task 9: `VmAppSender` + boot-path swap

**Files:**
- Create: `crates/ava-engine/src/networking/vm_app_sender.rs`
- Modify: `crates/ava-engine/src/networking/mod.rs`; `crates/avalanchers/src/wiring/chains.rs` (the **network** boot path around line 1353 — `boot_chain_over_network_core`'s `NoopAppSender`)
- Test: inline + `crates/avalanchers/tests/outbound_sender_boot.rs` extension

**Interfaces:**
- Produces: `pub struct VmAppSender<S: Sender>(Arc<S>)` implementing `ava_vm::AppSender` by direct delegation (`send_app_request(nodes, id, bytes)` → `Sender::send_app_request(&HashSet, id, bytes)`; same for response/error/gossip — the engine `Sender` app surface at `common/sender.rs:112-127` is signature-compatible). The loopback boot path (`chains.rs:418`) keeps `NoopAppSender` (no behavior change for existing boot tests).

- [ ] **Step 1:** Failing test in `outbound_sender_boot.rs` style: build `VmAppSender` over an `OutboundSender` backed by the recording mock `Network`; call `send_app_gossip(SendConfig{peers: 1, ..}, b"hi")`; decode the recorded wire message → `AppGossip{chain_id, app_bytes: "hi"}`.
- [ ] **Step 2:** Implement + swap the network-path `NoopAppSender`. Run `-p ava-engine -p avalanchers` green (all existing boot tests must stay green — the loopback path is untouched).
- [ ] **Step 3:** Commit: `feat(ava-engine): VmAppSender bridges ava-vm AppSender onto the engine OutboundSender; network boot path uses it`

---

### Task 10: `EvmMempool` remote admission + iteration

**Files:**
- Modify: `crates/ava-evm/src/mempool.rs`
- Test: existing inline test module

**Interfaces:**
- Produces:
  - `pub fn add_remote(&mut self, tx: RecoveredTx, sender: &SenderAccount, rules: &AdmissionRules) -> Result<B256, EvmMempoolError>` — identical validation to `add_local` (factor the shared body into a private `admit(tx, sender, rules, local: bool)`), marks `PoolEntry.local = false`.
  - `PoolEntry` gains `local: bool` (`add_local` ⇒ true).
  - `pub fn get(&self, hash: &B256) -> Option<RecoveredTx>` (clone out).
  - `pub fn iterate(&self, f: &mut dyn FnMut(&RecoveredTx) -> bool)` — all pooled txs, stop on `false`.
  - `pub fn take_gossip_outbox(&mut self) -> Vec<RecoveredTx>` — txs admitted (local **and** remote) since the last take; the push gossiper's drain source. (Deviation from coreth's txpool-subscription feed — documented in the method doc; same observable effect: newly admitted txs get pushed.)

- [ ] **Step 1:** Failing tests: `add_remote_dedups_against_local` (same hash → `AlreadyKnown`), `iterate_yields_all_pooled`, `take_gossip_outbox_drains_once` (two adds → outbox of 2 → second take empty), `get_returns_pooled_tx`.
- [ ] **Step 2:** Run (FAIL) → implement → `-p ava-evm` green (the full crate — mempool tests + the pipeline tests that construct `PoolEntry`).
- [ ] **Step 3:** Commit: `feat(ava-evm): EvmMempool remote admission, iteration, gossip outbox`

---

### Task 11: `ava-evm` gossip module — `GossipEthTx` + marshaller + `Set`

**Files:**
- Create: `crates/ava-evm/src/gossip.rs`
- Modify: `crates/ava-evm/src/lib.rs` (`pub mod gossip;`), `crates/ava-evm/Cargo.toml` (`ava-p2p` dep)
- Test: inline

**Interfaces:**
- Consumes: `ava_p2p::gossip::{Gossipable, Marshaller, Set, BloomSet}`; `EvmMempool` (Task 10); the `SenderAccount` resolution pattern from `rpc/eth.rs:296-320`.
- Produces:
  - `pub struct GossipEthTx(pub RecoveredTx);` — `gossip_id()` = `Id::from(<[u8;32]>::from(tx.hash().0))` (B256 → Id, both 32 bytes).
  - `pub struct EthTxMarshaller;` — `marshal` = `encode_2718`; `unmarshal` = `decode_2718` + `try_into_recovered` (Go `GossipEthTxMarshaller`, coreth `plugin/evm/gossip.go`).
  - `pub trait SenderAccountReader: Send + Sync { fn sender_account(&self, addr: &Address) -> Result<SenderAccount, Error>; }` — implemented in Task 12 by the VM's state handle (the `view_tip` + `read_account` pattern).
  - `pub struct EthTxGossipSet { mempool: Arc<parking_lot::Mutex<EvmMempool>>, accounts: Arc<dyn SenderAccountReader>, rules: AdmissionRules, bloom: parking_lot::Mutex<BloomSet> }` implementing `Set<GossipEthTx>`: `add` = resolve sender account → `mempool.add_remote` → `bloom.add` + `reset_if_needed` refilled from `mempool.iterate`; `has` = `mempool.contains`; `get_filter` = `bloom.marshal()`.
  - Bloom params: `TX_GOSSIP_BLOOM_MIN_TARGET_ELEMENTS`, `TX_GOSSIP_BLOOM_TARGET_FPP`, `TX_GOSSIP_BLOOM_RESET_FPP` — copy the exact values coreth passes (read `~/avalanchego/graft/coreth/plugin/evm/vm.go` + `gossip.go`; cite lines).

- [ ] **Step 1:** Failing tests with a fixture signed tx (reuse the mempool tests' tx builder helpers): `set_add_admits_valid_remote_tx`, `set_add_rejects_wrong_chain_id_without_poisoning` (bad tx → `Err`, then a good tx still admits), `get_filter_readable_and_contains_added` (`ReadFilter::parse` + `contains_key(gossip_id, salt)`), `marshaller_round_trips_2718`.
- [ ] **Step 2:** Run (FAIL) → implement → `-p ava-evm` green; clippy/fmt.
- [ ] **Step 3:** Commit: `feat(ava-evm): GossipEthTx marshaller + bloom-backed gossip Set over EvmMempool (coreth gossip.go parity)`

---

### Task 12: `EvmVm` wiring — p2p network, gossip system, handler/connector delegation

**Files:**
- Modify: `crates/ava-evm/src/vm.rs` (initialize + the `AppHandler`/`Connector` stub impls at `vm.rs:655-712`), `crates/ava-evm/src/gossip.rs` (the `SenderAccountReader` impl over the VM state handle)
- Test: inline in `vm.rs` tests or `crates/ava-evm/tests/gossip_vm.rs`

**Interfaces:**
- Consumes: everything above. `ChainContext` carries `node_id` (check the field name in `ava-snow`'s `ChainContext`).
- Produces:
  - `EvmVm::initialize` (after mempool/builder setup, mirroring coreth `vm.go:782-833` ordering): build `P2pNetwork::new(node_id, app_sender)`; build `EthTxGossipSet` (accounts reader = a small struct over the same state handle `rpc/eth.rs` uses); build `PushGossiper`/`PullGossiper`/`GossipHandler` with `GossipParams::default()`; `add_handler(TX_GOSSIP_HANDLER_ID, handler)`; spawn three `every(...)` loops (push cycle also drains `mempool.take_gossip_outbox()` into `push.add`) on a `CancellationToken` child stored in `Shared` and cancelled in `shutdown`.
  - `impl AppHandler for EvmVm` delegates all four methods to the stored `P2pNetwork` (late-bound: `ArcSwapOption<P2pNetwork>` on `Shared`, the `set_metrics` precedent — None before initialize ⇒ methods no-op with a `tracing::debug!`).
  - `impl Connector for EvmVm` delegates to `P2pNetwork` (peer set for pull sampling).
- **Ordering note for the implementer:** the app_sender arrives *at initialize* (`create_chain.rs:676`), so no boot-path capture dance is needed — this is why Task 9 only swapped the sender.

- [ ] **Step 1:** Failing test `gossip_vm.rs::inbound_push_gossip_lands_in_mempool`: initialize an `EvmVm::from_genesis` (the existing test-boot pattern — copy from an existing vm.rs test) with a recording mock `AppSender`; hand-build the frame `protocol_prefix(0) ++ PushGossip{gossip:[2718 tx bytes]}`; call `vm.app_gossip(&token, node, &frame)`; assert `mempool.contains(tx_hash)`.
- [ ] **Step 2:** Failing test `pull_request_answered_with_pool_contents`: seed the mempool via `add_local`; call `vm.app_request(&token, node, id, deadline, &frame)` where frame = prefix 0 + `PullGossipRequest` with an empty-ish filter; assert the recording `AppSender` got `send_app_response` whose `PullGossipResponse` contains the tx.
- [ ] **Step 3:** Failing test `local_submission_pushes_gossip` (paused time): initialize; `add_local` a tx via the RPC path or mempool handle; advance time past `push_period`; assert the recording `AppSender` saw `send_app_gossip` with the tx.
- [ ] **Step 4:** Run (FAIL) → implement → `-p ava-evm` green. **Note:** `-p ava-evm` tests are firewood-ethhash global-switch sensitive — run the crate single-threaded if the existing suite does (check `.config/nextest.toml` overrides; follow the M6 gotcha).
- [ ] **Step 5:** Commit: `feat(ava-evm): EvmVm wires tx gossip at initialize — P2pNetwork + push/pull system (coreth vm.go parity)`

---

### Task 13: `eth_getTransactionByHash` (pool + mined)

**Files:**
- Modify: `crates/ava-evm/src/rpc/eth.rs`, `crates/ava-evm/src/rpc/service.rs` (dispatch arm)
- Test: existing rpc service test module pattern

**Interfaces:**
- Produces: `EthBackend::get_transaction_by_hash(&self, hash: B256) -> Result<Value>`:
  - pool hit (`mempool.get`) → tx JSON object with `blockHash: null, blockNumber: null, transactionIndex: null` (geth pending shape; field set per coreth `internal/ethapi` `RPCTransaction` — include `hash`, `nonce`, `from`, `to`, `value`, `gas`, `gasPrice`/1559 fields, `input`, `type`, signature values `v`,`r`,`s`, `chainId`);
  - mined: resolve via the existing `tx_index`/receipt record path (the same lookup `get_transaction_receipt` uses at `rpc/eth.rs:340`) — reconstruct the tx from the canonical block's stored txs (find how receipts map to block number + index; read `receipts.rs` first);
  - unknown → `Value::Null`.

- [ ] **Step 1:** Failing tests: `get_transaction_by_hash_pending_has_null_block_hash` (submit via `send_raw_transaction`, then query — `blockHash` is `Value::Null`, `hash` matches); `get_transaction_by_hash_unknown_is_null`. A mined-path test if the existing rpc test harness already mines a block (check `service.rs` tests; if none mines, cover mined via the existing receipt-test fixture pattern or defer the mined leg's test to the live arm with a doc note).
- [ ] **Step 2:** Run (FAIL) → implement → `-p ava-evm` green.
- [ ] **Step 3:** Commit: `feat(ava-evm): eth_getTransactionByHash (pool-pending + mined)`

---

### Task 14: Offline two-node gossip e2e

**Files:**
- Create: `crates/avalanchers/tests/tx_gossip_two_node.rs`
- Reference: `crates/avalanchers/tests/two_node_convergence.rs` (copy its two-real-`Node` localhost-TLS bring-up verbatim as the fixture)

**Interfaces:**
- Consumes: the full stack Tasks 1–13.

- [ ] **Step 1:** Write `push_gossip_carries_tx_between_two_real_nodes`: boot nodes A and B through the production path (as `two_node_convergence.rs` does), wait for C-chain NormalOp on both; submit a raw signed transfer to **A**'s C-chain RPC (`eth_sendRawTransaction` — build the tx with the same fixture helper the rpc tests use, chain-id matching the local net); poll **B**'s `eth_getTransactionByHash` (bounded, e.g. 30 s) until it returns the tx with `blockHash: null`. That single assert proves: push emit (A), wire (real TLS), decode → adapter → VM → mux → handler → remote admission (B).
- [ ] **Step 2:** Write `pull_gossip_reconciles_when_push_missed`: same fixture, but construct node A's C-chain VM with `GossipParams{ push_period: Duration::from_secs(3600), .. }` (requires a test-only params override seam — add `EvmVm::set_gossip_params_for_test` or thread params through the existing config-bytes path; pick whichever is smaller and document); seed A's pool, assert B still converges within a few pull periods (poll ≤ 15 s).
- [ ] **Step 3:** Run: `./scripts/nix_run.sh cargo nextest run -p avalanchers -E 'test(tx_gossip)'` — green, plus the full `-p avalanchers` suite for regressions.
- [ ] **Step 4:** Commit: `test(avalanchers): two-real-node tx gossip e2e — push carries, pull reconciles`

---

### Task 15: Recorded Go-oracle wire goldens

**Files:**
- Create: `tests/vectors/p2p_sdk/` (committed goldens) + the env-gated Go emitter (copied into `~/avalanchego` per the M5–M8 recorded-oracle pattern; keep the Rust-side copy under `tests/differential/src/bin/` or the pattern the previous emitters used — find one: `grep -rn "recorded-oracle\|emitter" tests/ crates/ –l` and mirror it)
- Create: `crates/ava-p2p/tests/wire_goldens.rs`

**Interfaces:**
- Goldens: `push_gossip_frame.bin` (Go `p2p.PrefixMessage(ProtocolPrefix(0), marshaled PushGossip{gossip: [0xDE 0xAD], [0xBE 0xEF]})`), `pull_gossip_request.bin` (fixed salt/filter bytes), `pull_gossip_response.bin`.

- [ ] **Step 1:** Run `./scripts/check_oracle_binary.sh` — must print `OK` before capture.
- [ ] **Step 2:** Write the Go emitter (a `_test.go` gated on an env var, per the established pattern) producing the three files with **fixed** inputs; copy into `~/avalanchego`, run, copy outputs to `tests/vectors/p2p_sdk/`, commit the goldens + the emitter source under `tests/vectors/p2p_sdk/emitter/` (so it's re-runnable).
- [ ] **Step 3:** `wire_goldens.rs`: Rust builds the identical frames (`protocol_prefix` + prost encode with the same fixed inputs) and byte-compares against `include_bytes!` of each golden; also parses each golden back (decode leg).
- [ ] **Step 4:** Run `-p ava-p2p` green. Commit: `test(ava-p2p): Go-oracle wire goldens for prefixed PushGossip / PullGossip frames`

---

### Task 16: Live legs + `rust_proposes` proposer-ID rework

**Files:**
- Modify: `tests/differential/src/livenet.rs` (new helpers), `tests/differential/src/network.rs` (index-API extra_args), `tests/differential/tests/mixed_network.rs`
- Modify: `tests/differential/Cargo.toml` (add `ava-proposervm` dep for block parsing)

**Interfaces:**
- New helpers in `livenet.rs`:
  - `pub async fn await_c_pending_tx(api_base: &str, tx_hash: &str, timeout: Duration) -> Result<bool>` — polls `eth_getTransactionByHash`, true when the tx exists with `blockHash == null`.
  - `pub async fn c_block_number_of_receipt(api_base: &str, tx_hash: &str) -> Result<u64>`.
  - `pub async fn proposer_of_accepted_container(index_api_base: &str, index: u64) -> Result<ava_types::node_id::NodeId>` — POST `/ext/index/C/block` `{"method":"index.getContainerByIndex","params":{"index":"<n>","encoding":"hex"}}` (hand-rolled JSON-RPC over `tokio::net::TcpStream`, the `Observation::collect` precedent — **no http crate**); hex-decode container bytes → `ava_proposervm::block::codec::parse_without_verification` → post-fork `proposer()`; pre-fork/unsigned → `NodeId::default()`.
- Harness: the Go nodes in `boot_mixed_rust_validator` gain `--index-enabled=true` via the existing per-node `extra_args` mechanism (rung-3 precedent).

- [ ] **Step 1:** New live test `mixed_network_tx_gossip` (gated `#[cfg(feature="live")] #[ignore]`, env-guard on `AVALANCHEGO_PATH`, same skeleton as `mixed_network_rust_proposes` at `mixed_network.rs:265`):
  - Stage A (Go→Rust): `submit_c_transfer(&go_api)` → `await_c_pending_tx(&rust_api, …, 30 s)` must go true **before** the mined receipt appears on the Rust node (poll both; pending-first or simultaneously-pending proves gossip; if it mines Go-side within one poll interval before the Rust pool sees it, retry with a fresh tx up to 3×).
  - Stage B (Rust→Go): `submit_c_transfer(&rust_api)` → `await_c_pending_tx(&go_api, …, 30 s)` true.
  - Close: `await_same_c_height` on both + `Observation` no-fork check (copy from `mixed_network`).
- [ ] **Step 2:** Rework `mixed_network_rust_proposes` (`mixed_network.rs:265-345`): keep stages 1–2; replace the "no gossip path could have delivered it" comment + detection with: after `await_c_receipt(&rust_api, …)` is true, `let n = c_block_number_of_receipt(&rust_api, &tx_hash)`; `let proposer = proposer_of_accepted_container(&go_index_api, n - 1)` (container index = height − 1 on the linear C-chain — assert-and-log; if the parse or identity fails, scan indices `n−3 ..= n+1` and pick the container whose inner block, when the proposer matches, is logged for diagnosis); loop: while `proposer != rust_node_id` and attempts < 6, submit a fresh transfer and retry. Assert a Rust-proposed block eventually included one of our txs, then keep the existing Go-side receipt + same-height closing asserts.
- [ ] **Step 3:** Offline compile gate: `./scripts/nix_run.sh cargo nextest run -p ava-differential` (offline arms) green + `cargo build -p ava-differential --features live --tests` compiles.
- [ ] **Step 4 (operator/live):** Prewarm a freshly relinked release `avalanchers` (`--version` once; touch dep-crate lib.rs first — stale-binary gotcha), `./scripts/check_oracle_binary.sh` → OK, then run each leg via `cargo test --test mixed_network -- --ignored --exact --nocapture <name>` (nextest's 120 s slow-timeout kills live arms): `mixed_network_tx_gossip`, `mixed_network_rust_proposes`, and the follower `mixed_network` regression. All three must PASS.
- [ ] **Step 5:** Commit: `test(differential): live tx-gossip legs + rust_proposes proposer-ID detection (index API + proposervm parse)`

---

### Task 17: Closeout gate

**Files:** `BUILD.bazel` files (generated), `Cargo.lock`, `MODULE.bazel.lock`, plan/spec AS-BUILT notes.

- [ ] **Step 1:** `./scripts/run_task.sh bazel-gazelle-generate` (new crate + new tests) and `./scripts/run_task.sh bazel-check-metadata` — commit generated `BUILD.bazel`. Remember: metadata gates silently skip on a warm cache; run after all code tasks.
- [ ] **Step 2:** `./scripts/run_task.sh deps-tidy` (new `ava-p2p` edges; prost build-dep) — commit `Cargo.lock` + `MODULE.bazel.lock`.
- [ ] **Step 3:** `./scripts/run_task.sh lint-all` — clean (includes lint-determinism; add the allowlist entry only if it flags the bloom-salt site).
- [ ] **Step 4:** `./scripts/run_task.sh test-unit` — full workspace green (this is the cross-task gate that has caught rustfmt drift and TOCTOU races before; do not substitute scoped runs).
- [ ] **Step 5:** Fold AS-BUILT notes: `plan/M9-interop-hardening.md` (tx-gossip milestone landed; `rust_proposes` detection reworked), the design spec's `## AS-BUILT notes` section, and `specs/` touchpoints if any invariant text referenced "tx gossip deferred".
- [ ] **Step 6:** Commit: `docs: tx-gossip AS-BUILT — C-Chain push/pull gossip live, rust_proposes proposer-ID detection`

---

## Self-review notes (already applied)

- **Spec coverage:** bloom hoist (T1), sdk protos (T2), mux+client (T3/T4), gossip framework (T5/T6), engine App routing (T7), connected/disconnected (T8), AppSender swap (T9), mempool remote path (T10), C-Chain Set (T11), VM wiring (T12), `eth_getTransactionByHash` (T13), offline two-node e2e incl. pull-only (T14), Go-oracle wire goldens (T15), live legs + rust_proposes rework (T16), closeout (T17). The spec's "adapters forward in both states" is T7; "AppRequest timeout registration" already exists (`op::APP_REQUEST`, STEP p) — T7 only extends the synthesized `AppRequestFailed` with code/message.
- **Known simplifications (each documented in-module with Go cites):** `SendConfig`-based push targeting instead of stake-percentage `BranchingFactor` (our `OutboundSender`/`Network::gossip` resolves sampling); connected-peer pull sampling instead of validator-aware samplers; `take_gossip_outbox` instead of a txpool subscription feed. All are follower-of-Go in observable effect on the 5-node local net; validator-aware sampling joins the `PChainValidatorManager` follow-up.
- **Out of scope (unchanged from spec):** atomic-tx gossip (handler 1), migrating avm/platformvm/saevm `GossipTransport` seams, ACP-118, plugin-path inbound App routing.
