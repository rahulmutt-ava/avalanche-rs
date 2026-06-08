# M6.19 `differential::atomic_xc` — provenance

**Provenance: Go-EXECUTED (transitively).** `differential::atomic_xc`
(`crates/ava-evm/tests/atomic_xc.rs`) is a recorded-oracle **composite** parity
test. It introduces **no new golden bytes of its own**: every byte string it
asserts is read from the two sibling Go-EXECUTED vector sets that already live in
this tree. The composite's contribution is to drive a *single* ImportTx +
ExportTx corpus through all four X↔C atomic facets in one pass and check them
against the same Go authority.

## What is Go-authoritative here

| Facet | Source vector | Go authority |
|-------|---------------|--------------|
| (a) tx + component serialization, export signed-tx id | `../atomic/atomic_txs.json` | coreth `plugin/evm/atomic/{tx,import_tx,export_tx,codec}.go`, `go1.25.10` — see `../atomic/_provenance.md` |
| (b) atomic `Requests` (Import→Remove on source, Export→Put on dest) | `../atomic/atomic_txs.json` | `(*UnsignedImportTx).AtomicOps` / `(*UnsignedExportTx).AtomicOps` — same dump |
| (c) post-`EVMStateTransfer` balances/nonces | derived from `../atomic/atomic_txs.json` amounts × `X2C_RATE` | coreth `(*UnsignedImportTx).EVMStateTransfer` (credit `amount·X2CRate`) / `(*UnsignedExportTx).EVMStateTransfer` (debit `amount·X2CRate`, `SetNonce(addr, input.Nonce+1)` requiring `cur == input.Nonce`) — `import_tx.go:335`, `export_tx.go:313` |
| (d) atomic-trie root + per-chain serialized `Requests`, cross-chain Put/Remove | `../atomic_trie/atomic_trie_root.json` | coreth `plugin/evm/atomic/state/atomic_trie.go`, `chains/atomic/shared_memory.go` — see `../atomic_trie/_provenance.md` |

coreth pinned rev: `fb174e8925ba86e9ba5fd84eb4d6e5e8c23ffc11`.

## Facet (c) note

The post-`EVMStateTransfer` balances/nonces are computed from the **Go-golden tx
amounts** (import out = 4999 nAVAX, export in = 3000 nAVAX, nonce = 7 in
`../atomic/atomic_txs.json`) multiplied by the Go-golden `X2C_RATE` constant
(`= 1e9`, asserted equal to coreth's in `cchain_atomic_tx.rs`). The Rust
`AtomicStateHook` arithmetic is a verbatim transliteration of coreth's
`EVMStateTransfer` (multiply by `X2CRate`; `nonce = max(cur, input.nonce+1)`,
which equals `input.nonce+1` on a valid input where coreth requires
`cur == input.nonce`). No additional scratch Go test was needed; the avalanchego
tree was left git-clean.

## No new Go vectors

`new_go_vectors_generated: false` (see `manifest.json`). The existing committed
vectors already cover the import + export round-trip end-to-end, so per the task
guidance to prefer reuse, none were regenerated.
