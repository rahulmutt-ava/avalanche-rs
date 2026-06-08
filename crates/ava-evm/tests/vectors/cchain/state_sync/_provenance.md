# C-Chain state-sync leaf (range-proof) golden vector — provenance

**Provenance: Rust-EXECUTED, wire-format-anchored to Go.** The leaf
`range_proof` bytes in `account_leaf_range_proof.json` are produced by
`firewood::api::FrozenRangeProof::write_to_vec` (firewood git tag `v0.5.0`,
`ethhash` feature) via `EvmStateSyncServer::handle_leafs` over a fixed
single-account Firewood-ethhash state. They are **byte-identical** to what the Go
`firewood/syncer` serves, because the Go FFI path delegates to the SAME firewood
Rust serializer:

```
Go: (*ffi.RangeProof).MarshalBinary()      // proofs.go
      -> C.fwd_range_proof_to_bytes(handle) // firewood-go-ethhash/ffi v0.5.0
      -> firewood `FrozenRangeProof::write_to_vec`  // the Rust crate we call directly
```

So the `66776470726f6f66` (`"fwdproof"`) magic header + V0 framing here are the
exact bytes on the p2p wire (`ProofResponse.range_proof`,
`proto/sync/sync.proto`).

## How it was generated

A throwaway `#[ignore]`d test (`zz_dump_golden`) was added to
`crates/ava-evm/tests/state_sync.rs`, run, captured, then deleted:

```
cargo nextest run -p ava-evm -E 'binary(state_sync)' \
  --run-ignored all -E 'test(zz_dump_golden)' --no-capture
```

## Inputs

A single account committed through `FirewoodStateProvider`
(`hashed_post_state_to_batchops` -> `propose_and_stash` -> `commit`):

| Field   | Value                                        |
|---------|----------------------------------------------|
| address | `0x0101…01` (`Address::repeat_byte(0x01)`)   |
| nonce   | `1`                                          |
| balance | `1000` wei                                   |
| code    | empty (`bytecode_hash = None` → `KECCAK_EMPTY`) |

## Outputs

- `state_root` — the committed Firewood-ethhash root.
- `leaf_key` — `keccak256(address)` (the account-trie key, `account_key`).
- `leaf_value_rlp` — the libevm 5-field `StateAccount` RLP
  `[nonce, balance, storage_root(empty sentinel), code_hash(KECCAK_EMPTY),
  isMultiCoin=false]` (`rlp_account`, spec 10 §17.2.1). Note `0xf847…80`: the
  trailing `0x80` is the 5th `isMultiCoin=false` field.
- `range_proof` — the full-range (`start=end=nothing`) `FrozenRangeProof`
  serialization. Layout (firewood `proofs/ser.rs` V0):
  - `66776470726f6f66` — the `"fwdproof"` 8-byte magic.
  - `0001` — header (version 0, `ProofType::Range`).
  - `10` — proof framing.
  - … start/end proofs + the `(key, value)` pair.

## Source files exercised

- `crates/ava-evm/src/sync/server.rs` — `EvmStateSyncServer::handle_leafs`
  (`FirewoodStateView::range_proof_bytes` → `write_to_vec`).
- `crates/ava-evm/src/state.rs` — `account_key`, `rlp_account`,
  `hashed_post_state_to_batchops`.
- firewood `v0.5.0` `firewood/src/proofs/{ser,de,range}.rs`.

The `account_leaf_range_proof` test in `state_sync.rs` re-runs the same
construction and asserts the served bytes equal this committed vector — so a
firewood bump that changes the wire format trips the test.
