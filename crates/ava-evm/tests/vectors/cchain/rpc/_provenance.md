# `eth_*` RPC golden vectors (C-Chain / EVM, spec 10 §9.1/§17.9, G8, M6.23)

These vectors pin the JSON-RPC `eth_*` request → response shapes the `ava-evm`
`rpc::eth::EthRpc` handlers emit, in the Ethereum JSON-RPC encoding coreth's
`eth/` server uses (`0x`-quantity for numbers — `0x0` for zero; full-width
`0x`-data for byte strings/hashes — `0x` for empty). Each file is
`{ "request": <JSON-RPC 2.0 request>, "result": <expected result> }`; the
`request` field documents the call that produces `result` (the handler API takes
typed args, so the test constructs the call directly rather than parsing the
envelope).

## Why these are constructed-from-first-principles (not captured from a live coreth)

A live coreth C-Chain RPC capture is not available in this sandbox, so the
vectors are constructed against a **known, in-repo genesis/state** and are
**self-checking against the in-repo handlers + the reth/revm executor**:

- The state is seeded deterministically by `tests/rpc_eth.rs::setup`: a funded EOA
  `0x1111…11` (balance = 1 ether = `1e18` wei, nonce 7) and a contract `0x2222…22`
  whose runtime bytecode is the canonical "return 42" program
  `602a60005260206000f3` (`PUSH1 0x2a; PUSH1 0x00; MSTORE; PUSH1 0x20; PUSH1 0x00;
  RETURN`). Both are committed through the real Firewood-ethhash
  propose→stash→commit lifecycle (the same path `accept()` uses), so the reads are
  exercised end-to-end against Firewood, not a mock.
- The accepted head is a `CanonicalStore` advanced to height 5 via the real
  `append_canonical` writer (linear +1 acceptance).
- The chain spec is `AvaChainSpec::c_chain(1 /* mainnet */, Chain::from_id(43114))`.

Because the encodings are the standard go-ethereum/coreth `hexutil` forms and the
values are read back through the same Firewood/revm machinery the Go node uses,
the vectors agree with coreth by construction for the covered methods.

## Per-vector notes

| Vector | Value | How derived |
|---|---|---|
| `eth_chainId` | `0xa86a` | 43114 (C-Chain mainnet) as a `0x`-quantity. |
| `eth_blockNumber` | `0x5` | last-accepted height (the canonical tip = 5). |
| `eth_getBalance` | `0xde0b6b3a7640000` | 1 ether = `1e18` wei, read from Firewood. |
| `eth_getTransactionCount` | `0x7` | the seeded EOA nonce (7). |
| `eth_getCode` | `0x602a60005260206000f3` | the contract runtime bytecode, read from the code side store. |
| `eth_call` | `0x…002a` (32-byte word `0x2a` = 42) | a single read-only revm `transact` against the latest accepted Firewood state; the contract returns the word 42. |
| `eth_estimateGas` | `0x521a` (21018) | the revm `gas.total_gas_spent()` of the same call (21000 intrinsic + the runtime exec gas). **Recorded** from the in-repo revm executor; coreth's estimator binary-searches for the minimal limit — the search refinement is a documented follow-up (see report). |
| `eth_getProof` | account fields + **empty** proof arrays | `balance`/`nonce`/`codeHash` from direct Firewood reads; `storageHash` = `EMPTY_ROOT_HASH` and `accountProof`/`storageProof[].proof` = `[]` **until M6.25** wires Firewood proofs into `StateProofProvider::proof` (see below). |
| `eth_gasPrice` | `0x0` | the suggested next-block base fee from `feerules::base_fee` with the default (genesis / legacy, pre-AP3) fee state → `errNilBaseFee` → "absent" → 0. |
| `eth_maxPriorityFeePerGas` | `0x0` | the C-Chain priority tip is always 0 (the dynamic base fee prices congestion; no separate miner tip — coreth `SuggestTipCap` → 0). |
| `eth_feeHistory` | `oldestBlock 0x4`, three `0x0` base fees, two `0.0` ratios | the suggested base fee repeated over the range (per-block historical base fee from stored headers + real `gasUsedRatio` from receipts land with the reth-db history wiring, M6.24). |

## Accepted-block tag mapping (spec 10 §17.9)

`latest` / `safe` / `finalized` all resolve to the last-accepted height (5);
`pending` maps to `latest`; `earliest` → 0. Snowman acceptance is final, so there
is no pending/unsafe head (coreth `rpc_accepted`). Asserted directly in
`tests/rpc_eth.rs::accepted_tags_all_map_to_last_accepted_height`.

## `eth_getProof` completeness (M6.23 → M6.25 handoff)

The account fields are correct **today** (direct Firewood reads). The merkle proof
arrays (`accountProof`, `storageProof[].proof`) and the true per-account
`storageHash` depend on Firewood range/inclusion proofs owned by **M6.25**
(`state.rs::FirewoodStateView`'s `StateProofProvider::proof` /
`StorageRootProvider::storage_root` currently return `unsupported` / the empty-trie
sentinel). Until M6.25 lands, this handler returns **empty** proof arrays and the
empty-trie `storageHash`. When M6.25 wires the proofs, the handler reads them via
the existing `StateProofProvider::proof` seam and the proof arrays populate with no
RPC-layer change.

## `debug_traceTransaction` (deferred, M6.23)

Deferred: the prestate tracer needs a revm inspector that is not reachable behind
the `ava_evm_reth` facade (G0) without a heavy dep. The handler returns a
documented error naming `debug_traceTransaction`
(`tests/rpc_eth.rs::debug_trace_transaction_is_deferred`).

## Regenerating

The vectors are self-checking against the in-repo handlers. After an intentional
encoding/semantics change, run `cargo nextest run -p ava-evm --test rpc_eth`, read
the recorded value from a failing assertion, and update the matching `result`
field (the `eth_estimateGas` value in particular is the executor-recorded gas).

---

# `avax.*` RPC golden vectors (C-Chain / EVM, spec 10 §9.2/§17.9, G8, M6.24)

These vectors pin the JSON-RPC `avax.*` request → response shapes the `ava-evm`
`rpc::avax::AvaxRpc` handlers emit, matching coreth's `avax` service
(`plugin/evm/atomic/vm/api.go`) + `admin.go`/`health.go`. As with the `eth_*`
vectors these are **constructed from first principles** against an in-repo,
deterministic state and are **self-checking against the in-repo handlers** (no
live coreth capture is available in the sandbox). Files are
`{ "request": <JSON-RPC 2.0 request>, "result": <expected result> }`; the handler
API takes typed args, so the test constructs the call directly.

The golden atomic tx is the **same `UnsignedExportTx` fixture** the atomic-tx
codec golden vectors use (`tests/vectors/cchain/atomic/`, `src/atomic/tx.rs`):
`network_id 1`, `blockchain_id 0x11×32`, `destination_chain 0x33×32`, one
`EVMInput{0x02×20, 3000, AVAX(0xAA×32), nonce 7}`, one exported
`TransferOutput{3000, owners[0x05×20]}`. Signed (zero-credential `Sign`) it has:

- **`txID`** = `3zumDKZwsTxzxJmoTduDdipS2Cuz19b5XWCDVbXaZPh9ZwQ98` — CB58 of
  `sha256(signedBytes)` (the same id whose hex `06ceeed2…4fddc` the atomic-tx
  codec test pins).
- **signed bytes (checksummed hex, `formatting.Hex`)** = the `tx` field of
  `avax_getAtomicTx.json` / the `params.tx` of `avax_issueTx.json`. It is
  `0x` + `hex(codec.Marshal(0, Tx) ++ sha256(...)[28..32])`.

| Vector | Method | How derived |
|---|---|---|
| `avax_issueTx` | `avax.issueTx` | decode the checksummed-hex tx → `Tx::parse` → `AtomicMempool::add_local` → `{txID}` (the CB58 id). |
| `avax_getAtomicTxStatus` | `avax.getAtomicTxStatus` | the tx recorded in the accepted index at height 3 → `{status:"Accepted", blockHeight:"3"}`. `blockHeight` is a `json.Uint64` quoted decimal string. Processing/Dropped/Unknown branches are asserted directly in the test (mempool + unit test). |
| `avax_getAtomicTx` | `avax.getAtomicTx` | the accepted tx's signed bytes as checksummed hex + `{encoding:"hex", blockHeight:"3"}`. |
| `avax_getBlockByHeight` | `avax.getBlockByHeight` | the `CanonicalStore` body bytes at height 3 (seeded as `b"body-3"`) as checksummed hex → `0x626f64792d330129086f`. The block-bytes wire format (the coreth RLP+ExtData block, §9.3) is owned by M6.7/the builder; this pins the **envelope** (`{block, encoding}`) over whatever body bytes the store holds. |
| `avax_getUTXOs` | `avax.getUTXOs` | the **empty paginated** reply (`numFetched "0"`, `utxos []`, `endIndex{address:"", utxo:<empty id CB58>}`, `encoding "hex"`). |

## `avax.getUTXOs` completeness (deferred shared-memory fetch)

coreth's `GetUTXOs` reads `avax.GetAtomicUTXOs` — an **address-indexed**
shared-memory iterator over a source chain. `ava-vm`'s `SharedMemory` trait
(`ava_vm::components::avax::shared_memory`) exposes `apply` (the accept-side
put/remove the atomic backend uses) but **not** the indexed `GetAtomicUTXOs`
iterator the read API needs. So the handler validates the args (address count ≤
`maxGetUTXOsAddrs`, non-empty) and returns the **empty paginated envelope** —
the same shape coreth returns when no UTXOs match. Wiring the indexed fetch lands
with the shared-memory iterator (a future task); the reply envelope is stable.

## `avax.getAtomicTx` / `getAtomicTxStatus` accepted-tx index

coreth threads an `atomicstate.AtomicRepository` (txID → signed bytes + height)
that block-accept advances. Until the VM (`EvmVm`, M6.10) wires acceptance into a
durable repository, the handlers read an in-memory `AcceptedAtomicTxIndex` (the
accept-side `put` seam). The status precedence is coreth's `getAtomicTx`:
Accepted (durable, with height) > Processing/Dropped (mempool) > Unknown.

## `admin.*` + health

`admin.startCPUProfiler`/`stopCPUProfiler`/`memoryProfile`/`lockProfile`/
`setLogLevel` are **no-ops** in this build (the `profiler.Profiler` and dynamic
logger are node-assembly concerns, §12-node) and each returns coreth's
`api.EmptyReply` (`{}`). The node health endpoint (coreth `health.go`
`HealthCheck` → `(nil, nil)`) reports `{healthy:true, lastAcceptedHeight}` — the
extra detail is informational (coreth returns `nil` details today). These are
asserted directly in `tests/rpc_avax.rs` / the `avax.rs`/`admin.rs` unit tests,
no JSON vector.

## Scoping — direct handlers, not jsonrpsee

Like the `eth_*` handlers (M6.23), `AvaxRpc`/`AdminRpc` are plain handler structs
returning `serde_json::Value`, NOT a `jsonrpsee`/`reth-rpc` server. Spec §9.2's
"axum/JSON-RPC 2.0 … mount alongside or via ava-api's router" mount topology is
explicitly deferred to the 12-node milestone; the handler API is the seam that
mount would call.
