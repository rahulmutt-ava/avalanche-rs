# ava-vm-rpc — porting notes

Tracks the Go → Rust port of the rpcchainvm out-of-process gRPC plugin host/guest
(`vms/rpcchainvm`, `specs/07-vm-framework.md` §5). M3.24 lands the v45
reverse-dial handshake, the `proto/vm` `VM` service, the guest `VmServer<V>` +
`serve`/`serve_with_addr`, and the host `RpcChainVm` (the full `ChainVm` over the
wire). M3.25 lands the proxied callback services. Items below are deliberate
follow-ups, recorded so later milestones close them rather than re-deriving them.

## Go source

- `vms/rpcchainvm/runtime/subprocess` — the v45 reverse-dial handshake →
  [`crate::host::RpcChainVm::start`] (host side) + [`crate::guest::serve_with_addr`]
  (guest side) + [`crate::runtime`] (the `Runtime` service).
- `vms/rpcchainvm/vm_client.go` — `VMClient` → [`crate::host::RpcChainVm`]
  (`ChainVm` translated to `proto/vm` RPCs) + [`crate::host::block::RpcBlock`].
- `vms/rpcchainvm/vm_server.go` — `VMServer` → [`crate::guest::VmServer`].
- `version/constants.go RPCChainVMProtocol` (= 45) →
  [`crate::RPC_CHAIN_VM_PROTOCOL`].
- `proto/vm/vm.proto`, `proto/vm/runtime/runtime.proto`,
  `proto/{appsender,sharedmemory,validatorstate,warp,aliasreader}` →
  generated into `OUT_DIR` by `build.rs`, surfaced under [`crate::pb`].

## Handshake (M3.24 — DONE)

The handshake is the avalanchego **v45 reverse-dial** flow, **not** hashicorp
go-plugin (specs 00 §11.1.1, 07 §5.1): host binds runtime listener `R`, serves
`Runtime`, hands `R.addr` to the plugin via `AVALANCHE_VM_RUNTIME_ENGINE_ADDR`;
the plugin binds `V`, dials `R`, calls `Runtime.Initialize(45, V.addr)`; the host
asserts the version and dials `V`. Tested by `handshake_protocol_mismatch`,
`handshake_timeout`, `rust_host_rust_guest_roundtrip` (the in-process leg of the
four-way interop matrix). Linux `PR_SET_PDEATHSIG` is set in the audited
`unsafe` `pre_exec` in [`crate::host::subprocess`]; non-Linux relies on the
`ChildGuard` kill-on-drop.

## Faithful placeholders / deferred surface

1. **Host + guest `VM.Initialize` — ✅ DONE (M9.10/M9.11, 2026-06-15).** The host
   [`crate::host::RpcChainVm::initialize`] now stands up the `proto/rpcdb`
   `Database` server (`db_server_addr`) and an appsender callback server
   (`server_addr`) on ephemeral loopback (cancelled on `shutdown`/`Drop` via a
   `callback_shutdown` token), encodes the `ChainContext` + genesis/upgrade/config
   bytes + the two addrs into `InitializeRequest` (`chain_context_to_request`),
   sends `VM.Initialize`, and seeds the client-side last-accepted from the
   response. The guest [`crate::guest::VmServer::initialize`] dials both addrs
   back, builds the `RpcDatabase`/`RpcAppSender` proxies, maps the request →
   `ChainContext` (`request_to_chain_context`), and runs the inner VM.
   `tests/vm_initialize.rs::rust_host_initializes_rust_guest` exercises a VM that
   does a real `put`/`get` over the **proxied** db at `initialize`, then
   build→verify→accept over the wire.
   **DEFERRED to node-assembly:** the callback bundle at `server_addr` currently
   serves appsender only — the full Go bundle also serves sharedmemory /
   aliasreader / validatorstate / warp / `grpc.health`, which need concrete host
   impls supplied by the node-assembly path; and `InitializeRequest.network_upgrades`
   is sent `None` (the guest reconstructs the fork schedule from `network_id`)
   pending the proto `NetworkUpgrades` round-trip.

2. **`LastAccepted` is client-side state, not an RPC.** `proto/vm` has no
   `LastAccepted` RPC; Go tracks it in the `chain.State` decorator (seeded at
   Initialize/SetState, advanced on `block.Accept`). [`crate::host::RpcChainVm`]
   mirrors this with a shared `Arc<Mutex<Id>>`: seeded at `start` via a benign
   `SetState(UNSPECIFIED)` probe (and at `set_state`), advanced by
   [`crate::host::block::RpcBlock::accept`]. `SetState(UNSPECIFIED)` is treated by
   the guest as a no-op phase probe that only returns the snapshot.

3. **Block-decide error mapping.** `Block::verify/accept/reject` return
   `ava_snow::Result`, whose `Error` enum has no transport/remote variant. A
   transport failure during a decide op is surfaced as
   `ava_snow::Error::Multiple(vec![])` (a "critical remote error" — the engine
   halts the chain, matching Go's treatment of a decide error). **Recommended
   spec/central change:** add an `ava_snow::Error::Remote`/`Vm(String)` variant
   (or a `Box<dyn Error>` cause) so the underlying gRPC status survives; until
   then the cause is logged, not carried.

4. **`WithVerifyContext` probing.** `BuildBlockResponse.verify_with_context` /
   `BlockVerifyRequest.p_chain_height` are carried on the wire, but the plain
   `Block` trait does not expose whether a block opts into
   `WithVerifyContext`; the guest reports `false` for now
   (`guest::block_verify_with_context`). Wire it through once the per-block
   `WithVerifyContext` wrapper lands (M3.16 follow-up / M5 proposervm-driven VMs).

5. **Batched / state-sync VM RPCs.** The guest's `GetAncestors` /
   `BatchedParseBlock` return `UNIMPLEMENTED`, and the state-sync RPCs report
   "not implemented" (`ERROR_STATE_SYNC_NOT_IMPLEMENTED`) — faithful to a VM that
   does not implement those optional capabilities. The default
   `get_ancestors`/`batched_parse_block` *fallbacks* (Go free functions on a
   non-batched VM, `ava_vm::block::{get_ancestors, batched_parse_block}`) and the
   host-side `as_batched`/`as_state_syncable` probes are M3.25 follow-ups.

6. **HTTP handler proxying (ghttp).** `CreateHandlers`/`NewHTTPHandler` return no
   handlers (the workspace has no `tower`/`http` stack yet — see `ava-vm`
   `HttpHandler` note). `proto/http`/`proto/net`/`proto/io` are not vendored.

7. **`Gather` metrics.** The guest's `Gather` returns no `MetricFamily`s (no
   Prometheus registry is plumbed through the VM trait). `vm.proto` still imports
   the vendored `io/prometheus/client/metrics.proto` for wire parity.

8. **Graceful shutdown / signal handling.** Go's `rpcchainvm.Serve` drops
   SIGINT/SIGTERM until the host signals shutdown, then exits on SIGTERM.
   [`crate::guest::serve`] instead serves until its `CancellationToken` is
   cancelled (the in-process model); the SIGINT/SIGTERM dance is a real-binary
   follow-up (M3.28 / plugin packaging).

9. **`MAX_MESSAGE_SIZE`.** avalanchego's `grpcutils` uses `math.MaxInt` for the
   rpcchainvm channel; we pin 2 GiB ([`crate::MAX_MESSAGE_SIZE`]) — the practical
   ceiling for a single block / ancestor batch, well above the p2p limit.

## Proxied callback services (M3.25 — DONE)

[`crate::proxy`] ships, for each callback proto, a guest-side **client** (the
plugin dials) implementing the Rust trait and a node-side **server** wrapper (the
node serves). Symmetry: plugin dials, node serves (07 §5.3). Tested by
`rpcdb_roundtrip` + `appsender_roundtrip` (`tests/proxy.rs`).

- `proxy::rpcdb` — reuses `ava_database::rpcdb::{DatabaseClient, DatabaseServer}`
  (the `ErrEnumToError` table, server-side iterator handles, and batched
  `IteratorNext` already live in M1.11). `dial` is **synchronous** and owns the
  runtime so the channel's background task and the blocking `DynDatabase` RPCs
  share one runtime; call it from `spawn_blocking` / a dedicated thread, never
  inside an async runtime (04 §1.2). `serve` returns `DatabaseServer`; call
  `.into_service()` for the tower service.
- `proxy::appsender` — `RpcAppSender` (`AppSender`) ↔ `AppSenderServer`. Node-id
  sets are sorted before hitting the wire (00 §6.1).
- `proxy::sharedmemory` — `RpcSharedMemory` (`SharedMemory`, the sync trait, same
  blocking-runtime bridge as rpcdb) ↔ `SharedMemoryServer`. `apply` requests are
  emitted in `BTreeMap` chain-id order.
- `proxy::validatorstate` — `RpcValidatorState` (`ava_validators::ValidatorState`)
  ↔ `ValidatorStateServer`.

### Local-trait stubs (report — central owner needed)

- **`proxy::warp::Signer`.** The workspace has no warp message `Signer` trait
  (`ava_crypto::bls::Signer` is a different, lower-level BLS signer over raw
  bytes). A **minimal local** trait is defined here
  (`sign(network_id, source_chain_id, payload) -> Vec<u8>`). Replace it with the
  canonical warp `Signer` when the warp/crypto milestone lands.
- **`proxy::aliasreader::AliaserReader`.** The real `Aliaser`/`AliaserReader` is
  owned by `ava-chains` (M3.26). A **minimal local** trait is defined here
  (`lookup`/`primary_alias`/`aliases`); re-export / replace it from `ava-chains`
  once M3.26 lands.

### Public-key deserialization — ✅ CLOSED (M9.7, 2026-06-15)

`proto/validatorstate` carries BLS public keys as **uncompressed** 96-byte bytes
(`bls.PublicKeyToUncompressedBytes`). `proxy::validatorstate::decode_public_key`
now dispatches on length: `96 → PublicKey::from_uncompressed`,
`48 → PublicKey::from_compressed`, empty/other → `None`. `ava-crypto` already
exposed `from_uncompressed` (no central change needed). `tests/proxy_validatorstate.rs::validatorstate_proxy_matches_source`
asserts a real BLS key survives the round-trip losslessly.
**AS-BUILT correction:** the earlier "yields `None` for the uncompressed form"
claim was a *false positive* — `blst`'s `key_validate` auto-sniffs the
compression flag, so the old `from_compressed(96-byte)` path actually worked at
runtime. The fix makes the length dispatch explicit and removes the stale gap
wording; it is a correctness/clarity improvement, not a live bugfix.

### Recommended `ava_snow::Error` change (re-stated from item 3)

Add an `ava_snow::Error` variant carrying a remote/transport cause (e.g.
`Vm(String)` or a boxed source) so a gRPC failure during `Block::verify/accept/
reject` survives instead of collapsing to `Error::Multiple(vec![])`.
