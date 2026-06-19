# AVM genesis read/seed path — design (M5 follow-up #4)

Date: 2026-06-19
Status: approved (pre-implementation)
Scope: `crates/ava-avm`, `crates/ava-genesis` (re-import only)

## Problem

M5 left X-Chain genesis as a synthetic stand-in. `AvmVm::initialize` calls
`parse_genesis()`, which reads a 40-byte blob — `stop_vertex_id(32) ||
timestamp_secs(8)` — and feeds it straight to `State::initialize_chain_state`.
The VM never parses the real Go avm genesis format and never seeds the
genesis asset UTXOs; spendable UTXOs are only installed via the
`seed_genesis_state` test helper. This is M5 open follow-up #4 ("full Go
X-Chain genesis-asset parse").

The Go reference splits genesis into two phases:

- `vms/avm/vm.go:initGenesis(genesisBytes)` — decode `Genesis{Txs}` with the
  GenesisCodec, and for each asset: require top-level `Outs` empty, compute the
  txID, register the alias, and (on a fresh chain) seed `AddTx` + `AddUTXO` for
  each `tx.UTXOs()`. Index 0 establishes `feeAssetID` (the AVAX asset).
- `vms/avm/vm.go:Linearize(stopVertexID)` → `state.InitializeChainState(
  stopVertexID, time)` — builds the height-0 Snowman block. The stop-vertex
  id and time are **not** in the genesis bytes: they come from the upgrade
  config (`Upgrades.CortinaXChainStopVertexID`, `Upgrades.CortinaTime`).
  Local/default stop vertex is `ids.Empty`; mainnet/fuji are hardcoded
  historical vertex IDs.

## What already exists (do not rebuild)

- **Build side** (`ava-genesis/src/build.rs`): `AvmGenesis` / `AvmGenesisAsset`
  types, `new_avm_genesis` (the `NewGenesis` FixedCap subset), `avax_asset_id`
  (re-parse), marshaling via `AvmGenesisCodec()`.
- **ava-genesis wiring**: `from_config` → `avm_genesis_bytes` → `genesis_chains`.
- **ava-version**: `upgrade::get_config(network_id) -> UpgradeConfig` carrying
  `cortina_time` and `cortina_x_chain_stop_vertex_id`.
- **ava-avm**: `GenesisCodec()`, `CreateAssetTx`, `InitialState`, secp outputs,
  `State` with `add_utxo`/`add_tx`, `initialize_chain_state`, and the
  executor's CreateAssetTx UTXO production (EXEC-AVM-1 index continuation).

So every input exists; this work is wiring the VM read path and consolidating
the genesis types.

## Design

### Unit 1 — `ava-avm::genesis` (new module, canonical type home)

Mirrors Go `vms/avm/genesis.go`. Move `AvmGenesis`/`AvmGenesisAsset` here
(renamed `Genesis`/`GenesisAsset` in the `ava-avm` namespace); `ava-genesis`
re-imports them instead of defining its own. Single source of truth.

```rust
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct Genesis { #[codec] pub txs: Vec<GenesisAsset> }

#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct GenesisAsset { #[codec] pub alias: String, #[codec] pub tx: CreateAssetTx }

impl Genesis {
    /// `genesisCodec.Unmarshal(genesisBytes, &genesis)`.
    pub fn parse(bytes: &[u8]) -> Result<Genesis> { /* GenesisCodec().unmarshal */ }
}
```

The marshal helper the build side uses stays available (re-export or keep the
`GenesisCodec().marshal` call site in `ava-genesis`).

### Unit 2 — `UnsignedTx::utxos(tx_id)` (UTXO producer)

Faithful port of Go `vms/avm/txs/visitor.go:utxoGetter`: returns the UTXOs a tx
produces, with the running output-index continuation.

- BaseTx outs first: index `0..len(outs)`, asset = `out.asset_id()`.
- CreateAssetTx then appends each `InitialState`'s outs: index continues
  (`len(utxos_so_far)`), asset = `tx_id` (the asset is itself).

For a genesis CreateAssetTx, base `outs` is empty, so the produced UTXOs are
exactly the state outputs. Small, reusable, unit-testable. The executor may
adopt this producer later (out of scope here).

### Unit 3 — `AvmVm::initialize` genesis step (the `initGenesis` + `Linearize` port)

Replaces `parse_genesis` + the bare `initialize_chain_state` call:

```
genesis  = Genesis::parse(genesis_bytes)
upgrades = ava_version::upgrade::get_config(ctx.network_id)
for (index, asset) in genesis.txs:
    if !asset.tx.base.outs.is_empty(): return Error::GenesisAssetMustHaveState
    tx = Tx::new(UnsignedTx::CreateAsset(asset.tx)); tx.initialize(GenesisCodec())
    aliases.insert(tx.id(), asset.alias)
    if index == 0:
        debug_assert/log if tx.id() != ctx.avax_asset_id   // fee asset consistency
    if !state.is_initialized():
        state.add_tx(tx.id(), tx.bytes().to_vec())
        for utxo in tx.unsigned().utxos(tx.id()):
            state.add_utxo(utxo.id, utxo.bytes)
state.initialize_chain_state(
    upgrades.cortina_x_chain_stop_vertex_id,
    systemtime_from(upgrades.cortina_time),
    &codec())
```

- Ordering is load-bearing: asset seeding before chain-state init (matches Go).
- `is_initialized` guard makes re-open idempotent: assets are re-parsed (for
  aliases) but UTXOs seed only on a fresh chain — exactly Go's `stateInitialized`.
- `feeAssetID`: the VM keeps using `ctx.avax_asset_id` (the node computes it from
  the same bytes via `avax_asset_id()`); index 0 is asserted for consistency,
  not used to override.
- **Alias storage**: a VM-local `HashMap<Id, String>` mirroring Go `vm.Alias`,
  for the API's `lookupAssetID`. Not wired to the node `BCLookup`.

## Error handling

- Reuse `Error::GenesisAssetMustHaveState`, codec errors via `Error::Codec`,
  DB via `Error::Database`. The synthetic-seed path and its `Error::InvalidGenesis`
  short-slice checks are removed.
- `Genesis::parse` surfaces the codec error directly — no silent truncation
  (the M5.23 `p.errored()` discipline).
- Index-0 `avax_asset_id` mismatch is a `debug_assert` + logged warning, not a
  hard error: it can only differ on a programmer error, since the node derives
  `ctx.avax_asset_id` from the same bytes.

## Testing (round-trip + Go-oracle differential)

1. **Unit** — `Genesis::parse` round-trips `AvmGenesisCodec().marshal(...)`;
   `utxos()` index-continuation byte-exact vs Go `utxoGetter` (genesis
   base-outs-empty case + a multi-state case with non-empty base outs).
2. **Round-trip integration** — `ava_genesis::from_config` builds bytes →
   `AvmVm::initialize` parses → assert seeded UTXO set (ids + amounts + owners),
   `feeAssetID == avax_asset_id(bytes)`, alias `"AVAX" → tx_id`, and genesis
   block id/timestamp == `cortina_*` for local.
3. **Go-oracle differential** — recorded avalanchego avm genesis bytes (local +
   a fuji/mainnet sample) under `tests/`; assert Rust parse yields the same
   asset txIDs + UTXO ids. Established recorded-oracle pattern (env-gated emitter
   copied into `~/avalanchego`, live binary backs the recording).
4. **Regression** — existing `vm_conformance!` battery and the `seed_genesis_state`
   helper stay green (the helper remains for tests needing arbitrary UTXOs).

## Out of scope (follow-ups)

- nftfx/propertyfx genesis outputs — only secp `TransferOutput`/`MintOutput`
  are reachable; needs follow-up #6's typed fx outputs in `components.rs`.
- `_upgrade_bytes` overlay parsing — defaults-from-`network_id` only for now
  (Go threads `vm.Config.Upgrades` from upgrade bytes over the defaults).
- Executor adopting the shared `utxos()` producer (it has its own produce path).
- Node `BCLookup` alias wiring.
