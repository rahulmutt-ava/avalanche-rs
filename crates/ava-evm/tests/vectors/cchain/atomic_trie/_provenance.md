# C-Chain atomic-trie root golden vector — provenance

**Provenance: Go-EXECUTED** against the coreth `plugin/evm/atomic/state`
package on `go1.25.10 darwin/arm64`. The root is NOT hand-derived.

## How it was generated

A scratch Go test `zz_golden_dump_test.go` was placed in the coreth
`plugin/evm/atomic/state` package and run, then deleted:

```
cd /Users/rahul.muttineni/avalanchego/graft/coreth
go test ./plugin/evm/atomic/state/ -run TestGoldenDump -v
```

The test lives in `package state` so it can use the unexported `newTestAtomicTrie`
helper, `AtomicTrie.OpenTrie`/`UpdateTrie`, and the `TrieKeyLength` constant.

Module: `github.com/ava-labs/avalanchego/graft/coreth`
Source files exercised:
- `plugin/evm/atomic/state/atomic_trie.go` — `AtomicTrie.UpdateTrie`
  (`Codec.Marshal(CodecVersion, requests)` per `map[ids.ID]*Requests` entry;
  key = `Packer.PackLong(height) ‖ PackFixedBytes(blockchainID)`),
  `TrieKeyLength = wrappers.LongLen + common.HashLength = 8 + 32 = 40`,
  `types.EmptyRootHash`.
- `plugin/evm/atomic/codec.go` — `atomic.Codec`, `CodecVersion = 0`.
- `chains/atomic/shared_memory.go` — `Requests{RemoveRequests [][]byte;
  PutRequests []*Element}`, `Element{Key, Value, Traits}` (serialize order).

## Inputs (the M6.14 golden atomic ops, height = 1)

The merged per-chain op map for a block carrying the golden import + export tx:

| chain | Requests |
|-------|----------|
| source 0x22×32 | `RemoveRequests = [import_input_id]` |
| dest   0x33×32 | `PutRequests = [Element{key, value, traits=[0x05×20]}]` |

where `import_input_id`, the export `key`/`value`/`traits` are the exact bytes
from `tests/vectors/cchain/atomic/atomic_txs.json` (M6.14, also Go-executed).

## Captured facts

- `TrieKeyLength = 40`.
- `EmptyRootHash = 56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421`.
- atomic-trie root after indexing both ops at height 1:
  `15211e79c52a022d51afc4ed1cd77db2477cbcb85620d28a15923c5f96476056`.
- per-chain serialized `Requests` (the trie VALUE): the 2-byte codec version
  prefix `0000`, then `RemoveRequests` (`u32` count, each `u32` len + bytes),
  then `PutRequests` (`u32` count, each Element = `u32`-len key, `u32`-len value,
  `u32`-count traits each `u32`-len + bytes). See the `value` fields in
  `atomic_trie_root.json`.
