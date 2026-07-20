# C-Chain tx gossip — `ava-p2p` SDK port + EvmMempool wiring (design)

**Date:** 2026-07-20
**Status:** approved design, pre-plan
**Milestone context:** the tx-gossip subsystem deferred at the M9.15
rust-as-proposer merge (`74625f5`; "TX GOSSIP DEFERRED — its own milestone;
needs engine-layer AppGossip/AppRequest routing — InboundOp has no App
variants"). Go oracle pin at design time: `~/avalanchego` (rpcchainvm=45).

## Goal

Coreth-parity C-Chain transaction gossip, **push + pull**, proven live: a tx
submitted to a Go node lands in the Rust node's `EvmMempool` (and vice versa),
and the existing proposer path mines it with zero new proposal wiring.
Includes the `mixed_network_rust_proposes` detection rework that gossip
necessitates.

## Why now / what exists

- **Outbound is done.** `ava-engine::networking::sender::OutboundSender`
  already implements `send_app_request` / `send_app_response` /
  `send_app_gossip` over `ava_network::Network` (STEP o/p/q).
- **Traits are done.** `ava-vm` has `AppHandler` (`app.rs`), `AppSender`
  (`app_sender.rs`), and `Connector` (`connector.rs`), all threaded through
  `create_chain.rs` middleware.
- **The gaps:** engine `InboundOp` has no App variants and
  `ava-node/src/init/inbound_decode.rs` drops `AppGossip`/`AppRequest`/
  `AppResponse`/`AppError` in its ignore arm; there is no Rust analog of Go's
  `network/p2p` + `network/p2p/gossip` SDK — four VM crates (`ava-avm`
  `network/gossip.rs`, `ava-platformvm` `network.rs`, `ava-saevm-txgossip`,
  `ava-saevm-cchain` `gossip.rs`) each carry a local, never-wired
  `GossipTransport` seam; `ava-evm`'s `EvmMempool` (rust-as-proposer branch)
  has no gossip at all.

## Scope

**In:** the `ava-p2p` crate (protocol mux, client, gossip framework, sdk
protobufs), engine/node inbound App routing, C-Chain eth-tx gossip wiring
(handler ID 0) in `ava-evm` + chain boot, the bloom hoist to `ava-utils`,
offline + recorded-oracle + live tests, `rust_proposes` proposer-ID detection
rework.

**Out (follow-up slices):** atomic-tx gossip (handler ID 1); migrating the
avm/platformvm/saevm `GossipTransport` seams onto `ava-p2p`; ACP-118
signature-request handler; plugin-path (`ava-vm-rpc`) inbound App routing to
out-of-process VMs; validator-set-aware gossip sampling beyond the
connected-peer set (see Risks).

## Approach (chosen: faithful layered port)

Mirror Go's layering: the SDK sits on the abstract `AppSender`/`AppHandler`
traits, not the transport. Rejected alternatives: (a) bespoke gossip inside
`ava-evm` — every later VM re-implements it and the structure diverges from
the Go sources the specs key off; (b) extending `ava-network` — VMs would
depend on the whole transport crate (TLS, peers, dialer) just to gossip,
inverting Go's layering.

## Architecture

### New crate `crates/ava-p2p`

Deps: `ava-types`, `ava-utils`, `ava-vm`, `prost`, `tokio`, `thiserror`
(+ `tokio-util` for `CancellationToken`). Dependency direction:
`ava-evm → ava-p2p → ava-vm` (acyclic; `ava-p2p` never touches
`ava-network`). Modules mirror Go files:

| Module | Go source | Contents |
|---|---|---|
| `handler.rs` | `network/p2p/handler.go` | `Handler` trait (`app_gossip(node, bytes)`, `app_request(node, deadline, bytes) -> Result<Vec<u8>, AppError>`); `TX_GOSSIP_HANDLER_ID = 0`, `ATOMIC_TX_GOSSIP_HANDLER_ID = 1` (Go iota) |
| `network.rs` | `network/p2p/network.go` | `P2pNetwork`: implements `ava-vm` `AppHandler` + `Connector`; strips the **varint handler-ID prefix**, dispatches to registered handlers; tracks connected peers for sampling; `add_handler(id, handler) -> Client` |
| `client.rs` | `network/p2p/client.go` | per-handler `Client`: prefixes the varint ID, `app_request(nodes, bytes, on_response)` correlates `request_id → callback`, synthesizes the failure callback on `AppRequestFailed`; `app_gossip(cfg, bytes)` |
| `gossip/mod.rs` | `gossip/gossip.go`, `set.go` | `Gossipable` (`gossip_id() -> Id`), `Marshaller`, `Set` (`add`, `iterate`, `get_filter`) traits |
| `gossip/bloom.rs` | `gossip/bloom.go` | `BloomSet`: salt + `Filter` writer over the hoisted `ava-utils::bloom`; reset-on-saturation rebuilds from `Set::iterate` with a fresh salt |
| `gossip/push.rs` | `gossip/gossip.go` (PushGossiper) | 100 ms cadence; new-tx + regossip queues (size-bounded per coreth caps) |
| `gossip/pull.rs` | `gossip/gossip.go` (PullGossiper) | 1 s cadence; samples a connected peer; `PullGossipRequest{salt, filter}` via `Client` |
| `gossip/handler.rs` | `gossip/handler.go` | answers pulls: parse requester bloom, `iterate` the `Set`, return txs not in the filter, capped at Go's target response size |
| sdk protobufs | `proto/sdk/sdk.proto` | `PushGossip`, `PullGossipRequest`, `PullGossipResponse` via the repo's existing build.rs prost pipeline (generated, uncommitted) |

Gossip cadences reuse the constants already pinned in
`ava-saevm-txgossip` (`PUSH_GOSSIP_PERIOD` = 100 ms, `PULL_GOSSIP_PERIOD` =
1 s), sourced per-VM from coreth config defaults at wiring time.

### Bloom hoist

Move `crates/ava-network/src/network/bloom.rs` (byte-exact `utils/bloom` port,
already has both `ReadFilter` and the `Filter` writer) to
`ava-utils::bloom`; `ava-network` re-exports so its peer-list callers are
unchanged. This is the refactor the module's own doc comment reserved at
M2.17.

### Engine + node inbound routing

- `ava-engine/src/networking/router.rs`: new `InboundOp::{AppRequest
  {request_id, deadline, bytes}, AppResponse{request_id, bytes},
  AppGossip{bytes}, AppRequestFailed{request_id, error_code, message}}`
  (sender `NodeId` travels however the existing ops carry it).
- `ava-node/src/init/inbound_decode.rs`: decode the four App wire messages
  (today in the ignore arm) — kept the exact inverse of `OutboundSender`,
  extending the existing exhaustive-match convention (no `_ =>`).
- Both engine adapters (`BootstrapperEngineAdapter` + `SnowmanEngineAdapter`)
  forward App ops to the VM's `AppHandler` — Go delivers app messages to the
  VM in both states, so gossip flows during bootstrap too.
- `AppRequest` we *issue* registers with the `Router`/timeout manager like
  other request ops (STEP p precedent) so `AppRequestFailed` synthesizes on
  non-response.

### C-Chain wiring (`ava-evm` + chain boot)

- `ava-evm/src/gossip.rs`: `GossipEthTx` (gossip ID = tx hash), EIP-2718
  `Marshaller`, and an `EthTxGossipSet` implementing `gossip::Set` over
  `Arc<Mutex<EvmMempool>>` + `BloomSet` — `add` = remote admission path,
  `iterate` = pull answers, bloom insert on admission.
- `EvmMempool` gains: `add_remote` (same admission rules as `add_local`;
  origin tracked for regossip policy) and an `iterate` accessor. The existing
  `subscribe()` `Notify` wakes the `PushGossiper`.
- Chain boot (`ava-chains`/`avalanchers`): construct `P2pNetwork` over a small
  adapter exposing `ava-vm::AppSender` backed by the chain's engine `Sender`
  (`send_app_*` already exist); register handler 0; spawn push/pull loops on
  the chain's task set, `CancellationToken`-tied. `EvmVm` implements
  `AppHandler` + `Connector` by delegating to its `P2pNetwork`.

## Data flow

- **Push out:** `eth_sendRawTransaction` → `add_local` → `Notify` →
  `PushGossiper` batches → `PushGossip` → varint-`0` prefix →
  `AppSender::send_app_gossip` → `OutboundSender` → wire. Valid received txs
  enter the regossip queue (coreth forwarding behavior).
- **Push in:** peer `AppGossip` → `decode_inbound` → `InboundOp::AppGossip` →
  adapter → `EvmVm` `AppHandler` → `P2pNetwork` mux → gossip handler →
  unmarshal → `Set::add` → mempool remote admission + bloom insert. The
  existing `PendingWorkWaiter`/forwarder/`build_block` path picks the tx up
  unchanged — gossip terminates at the mempool.
- **Pull:** `PullGossiper` samples a connected peer each period, sends
  `PullGossipRequest{salt, filter}`; the responder's pull handler returns txs
  not in the requester's bloom, size-capped. Saturated blooms rebuild from
  current mempool contents with a fresh salt.

## Error handling

- A bad peer must never error the chain handler: malformed varint prefix,
  undecodable protobuf, or unknown handler ID on `AppGossip` → drop + metric.
- Unknown handler ID or malformed bloom on `AppRequest` → `AppError` reply
  with Go's exact codes (`network/p2p/error.go`).
- Per-tx admission failure inside a gossip batch skips that tx only.
- Gossip loops: `CancellationToken`-tied, injected clock (determinism gate;
  no wall-clock reads on consensus paths). The bloom-salt RNG gets a
  `xtask/determinism-allowlist.toml` entry (non-consensus; `tracked_ip`
  jitter-seed precedent).
- Regossip queues and pull responses are size-bounded (coreth caps).

## `rust_proposes` detection rework

Gossip breaks the current mechanism (tx submitted only to the Rust node ⇒
inclusion proves Rust built the block — any node can now mine it). New
detection: **proposer NodeID from the accepted proposervm block** — enable the
index API on one Go node in the harness, fetch the accepted C-chain proposervm
container for the block that included the tx, parse the proposer NodeID with
`ava-proposervm`, and retry (bounded) until a Rust-proposed block includes a
submitted tx. Ground truth that survives gossip; no test-only kill-switch
flag.

## Testing

1. **Unit (`ava-p2p`):** mux varint round-trip; client correlation + failure
   synthesis; `BloomSet` reset semantics; push/pull gossipers under paused
   tokio time with a mock `AppSender`.
2. **Recorded Go-oracle byte-parity:** goldens for `PushGossip` /
   `PullGossipRequest` / `PullGossipResponse` and the varint-prefixed frame,
   emitted from `~/avalanchego` (M5–M8 env-gated emitter pattern) — wire
   compat pinned offline. Run `./scripts/check_oracle_binary.sh` before any
   oracle capture.
3. **Engine/node:** `decode_inbound` App-op coverage (inverse of
   `OutboundSender`, extending the existing round-trip tests); adapters
   forward to the VM `AppHandler` in both Bootstrapper and Snowman states.
4. **Offline two-node e2e:** `two_node_convergence`-style harness — tx added
   on node A appears in node B's mempool via push; a pull-only variant (push
   disabled) proves the pull loop independently.
5. **Live gated legs** (`#[cfg(feature="live")] #[ignore]`,
   `$AVALANCHEGO_PATH`, prewarm + `check_oracle_binary.sh` rules apply).
   Careful assert design — mined-receipt alone proves nothing about gossip
   (Go mines a Go-submitted tx without Rust's help, and Rust sees the receipt
   as a follower):
   (a) **Go → Rust:** submit to a Go node, poll the **Rust node's**
   `eth_getTransactionByHash` until it returns the tx with `blockHash: null`
   (in-pool = gossip delivered) — requires the Rust RPC to answer
   `eth_getTransactionByHash` from the pool (small `ava-evm` RPC addition if
   absent); then confirm eventual mining, no fork.
   (b) **Rust → Go:** submit to the Rust node, poll a **Go node's**
   `eth_getTransactionByHash` for the pending (`blockHash: null`) state —
   Go can only have it pre-mining via Rust's push/pull.
   (c) reworked `rust_proposes` (proposer-ID detection);
   (d) existing follower `mixed_network` leg regression.
6. **Closeout:** full-workspace `lint-all` + `test-unit`, gazelle/bazel
   metadata regen (explicit `members` listing for the new crate — rules_rust
   cold-splice gotcha), `deps-tidy` for the new crate's edges.

## Risks / open items

- **Peer sampling set:** Go's pull gossip samples via validator-aware
  samplers; this slice samples the connected-peer set tracked by `Connector`
  events (sufficient for the 5-node local net where all peers are
  validators). Validator-set-aware sampling joins the follow-up list with
  `PChainValidatorManager` wiring.
- **`Connector` delivery:** the design assumes engine `connected`/
  `disconnected` events reach the VM through the existing `Connector`
  middleware; verify at plan time and wire the gap if the engine side never
  calls it.
- **Pending-tx visibility for live assert (a):**
  `eth_getTransactionByHash` answering from the pool may not exist in
  `ava-evm`'s RPC yet (only `eth_getTransactionReceipt` /
  `eth_sendRawTransaction` are known-present) — if absent, add the
  pool-reading handler as part of this milestone (it is also deferred
  M9.15 follow-up (4)'s sibling).
- **`InitiallyActiveTime` genesis nets activate everything:** gossip has no
  fork gating in coreth (protocol-level, not consensus), so no upgrade-schedule
  risk.
