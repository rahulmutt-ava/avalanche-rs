# Firewood ethhash golden vectors — provenance

`accounts_root.json` is a **REAL** Go-extracted vector (spec 04 §6.6, 02 §6).

## How it was produced

A scratch Go program drove
`github.com/ava-labs/firewood-go-ethhash/ffi v0.5.0` — the cgo bindings over the
**same** firewood Rust library the avalanche-rs `firewood` crate wraps (pinned to
git tag `v0.5.0`, rev `0695b91f` / annotated-tag object
`9fc632312e75b055a65a65f297ba26d92070893d`). The program:

1. opened a firewood DB with `ffi.EthereumNodeHashing` (the Keccak-256 +
   Ethereum-MPT/RLP mode),
2. recorded the empty-trie root,
3. applied the fixed batch in `accounts_root.json` (`batch`) via `db.Update`,
   where each entry is `Put(key, value)`,
4. printed the resulting state `root`.

The batch is three RLP-encoded `[nonce, balance, storageRoot, codeHash]` accounts
at "account depth" (32-byte keys) plus two storage-slot-shaped entries, all with
fixed deterministic bytes.

The Rust golden test (`tests/golden_firewood_ethhash.rs`) feeds the **identical**
`(key, value)` bytes to the `firewood` crate in `ethhash` mode and asserts the
computed root equals `root`. Because both sides drive the same firewood library,
matching roots prove the avalanche-rs wrapper reproduces the Go node's EVM state
root byte-for-byte on identical input.

## Versions / revisions

- `firewood-go-ethhash/ffi`: `v0.5.0`
- `firewood` Rust crate: git tag `v0.5.0` (rev `0695b91f`)
- avalanchego reference HEAD: `fb174e8925`
- Go toolchain: `go1.25.9` (darwin/arm64), prebuilt static lib
  `libs/aarch64-apple-darwin/libfirewood_ffi.a` shipped in the ffi module.

## Empty-trie root

`empty_root` = `56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421`
— the well-known Ethereum empty-trie / `types.EmptyRootHash` constant, which
firewood returns from `HashKey::default_root_hash()` in ethhash mode.

The scratch program was deleted after extraction (per the task recipe).
