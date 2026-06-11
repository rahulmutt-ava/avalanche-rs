# `ava-genesis` golden vectors — Go `genesis.FromConfig` dumps

Provenance: emitted from avalanchego @
`cc3b103b91173f5e8b89b1b31aea0816766c8ada` (Go 1.25.10, CGO_ENABLED=1) by
`crates/ava-genesis/tests/go-oracle/genesis_dump_oracle_test.go` (the env-gated
`TestEmitGenesisVectors`, dropped into the Go tree's `genesis/` package).
Re-freeze with `cargo xtask gen-genesis` (avalanchego checkout at
`../avalanchego`, override with `AVALANCHEGO_DIR`).

| File | Contents |
|---|---|
| `block_ids.json` | per-network golden IDs (P genesis block id = `sha256(p_bytes)`, X/C blockchain ids = `CreateChainTx` ids, AVAX asset id, sha256 hex + length of the byte stream) + the emitting commit |
| `p_chain_bytes_{mainnet,fuji,local_unmodified,custom_9999}.bin` | the full `genesis.FromConfig` P-Chain genesis byte streams (specs 23 §3) |
| `genesis_test.json` | the custom networkID-9999 config, copied **verbatim** from Go `genesis/genesis_test.json` (`TestGenesisFromFile`'s fixture) |

`local_unmodified` is the **pre-start-time-advance** local config
(`unmodifiedLocalConfig`) — the golden identity of specs 23 §7; the live local
config advances `startTime` in 9-month chunks (specs 23 §5.1) and so has no
fixed id. Consumed by `tests/golden_genesis_block_id.rs`
(`genesis_block_id` — the per-PR exit gate — and
`genesis_p_chain_bytes_byte_identical`, specs 23 §9.1/§9.2). An unexpected
change to any of these files is a **compatibility break**, not a routine
snapshot update (specs 02 §6.1).
