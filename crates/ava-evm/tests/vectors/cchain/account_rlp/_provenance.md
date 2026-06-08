# Account-RLP golden vectors (C-Chain / EVM, spec 10 §17.2.1)

These vectors pin the **standard Ethereum account-leaf RLP** that Firewood stores
(in ethhash mode) as the value at an account node, keyed by `keccak256(addr)`.
The encoding is `RLP([nonce, balance, storage_root, code_hash])` — exactly
go-ethereum's / coreth's `types.StateAccount` and reth/alloy's
`alloy_consensus::TrieAccount` (a 4-field `RlpEncodable` list). This is the same
shape the Go node's `firewood-go-ethhash` bindings write, so a single canonical
encoding is shared across the Go and Rust nodes.

## How each vector was produced / verified

`rlp` is `alloy_rlp::encode(TrieAccount { nonce, balance, storage_root,
code_hash })` via this crate's `state::rlp_account` (which fixes
`storage_root = EMPTY_ROOT_HASH`, matching how Firewood-ethhash treats the
persisted leaf bytes — the true storage root is recomputed from the sub-trie at
hash time, see `firewood/storage/src/hashers/ethhash.rs`). The golden test
(`state::tests::account_rlp_golden_vector`) re-encodes the documented fields and
asserts byte-equality with `rlp`, and decodes `rlp` back to the documented
`nonce`/`balance` via `state::decode_rlp_account`. So the vector is self-checking
against the in-repo encoder/decoder, and the encoder is the standard
alloy/go-ethereum account RLP — i.e. it agrees with the Go ethhash bindings by
construction (same RLP list, same field order).

The two sentinel constants embedded in the vector are the canonical Ethereum
values (also emitted by the Go node):

- `storage_root` = `EMPTY_ROOT_HASH`
  = `0x56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421`
  (keccak256 of the RLP empty string — go-ethereum `types.EmptyRootHash`).
- `code_hash` = `KECCAK_EMPTY`
  = `0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470`
  (keccak256 of the empty byte string — go-ethereum `types.EmptyCodeHash`).

## Vectors

- `eoa_one_ether.json` — an externally-owned account: nonce 0, balance
  1 ether (1e18 wei), no code (empty storage/code roots).

To regenerate after an intentional encoding change, print
`hex::encode(rlp_account(nonce, balance, code_hash))` and update the `rlp` field.
