# `ava-genesis` — Go → Rust porting matrix

Tracks coverage of Go `genesis/...` tests (specs 02 §13). Rows are seeded from
`go test -list '.*' ./genesis/` (10 entries) against the `../avalanchego`
reference tree @ `cc3b103b91173f5e8b89b1b31aea0816766c8ada`.

Legend: ⬜ not ported · 🟡 partial · ✅ ported

| Go test | Status |
|---|---|
| `TestAllocationCompare` | ✅ covered structurally: `Allocation::compare` is exercised end-to-end by `golden_genesis_block_id.rs::genesis_p_chain_bytes_byte_identical` (the X-alloc sort is byte-load-bearing) |
| `TestValidateConfig` | ✅ `validate.rs::tests::validate_config_table` (M8.6; + the duplicate-avaxAddr-allowed and empty-message cases) |
| `TestGenesisFromFile` | 🟡 the custom(9999) build + golden hash is in `golden_genesis_block_id.rs::genesis_block_id`; std-network rejection in `validate.rs::tests::standard_networks_rejected`; the missing-file/invalid-JSON rows in `validate.rs::tests::config_content_loader` (content form) |
| `TestGenesisFromFlag` | 🟡 `validate.rs::tests::{standard_networks_rejected,config_content_loader}` (`from_flag` composes the same loaders + `validate_config` + `from_config`) |
| `TestGenesis` | ✅ `tests/golden_genesis_block_id.rs::genesis_block_id` (M8.8 per-PR exit gate) + `genesis_p_chain_bytes_byte_identical` (full byte-stream parity vs the committed Go dumps, specs 23 §9.2) |
| `TestVMGenesis` | ✅ `tests/golden_genesis_block_id.rs::{genesis_block_id,vm_genesis_unknown_vm}` |
| `TestAVAXAssetID` | ✅ `build.rs::tests::avax_asset_id_matches_go` (M8.7) |
| `TestSampleBootstrappers` | ⬜ M8.14 (`bootstrappers.rs::tests::bootstrapper_parity`) |
| `TestGetRecentStartTime` | ⬜ M8.14 (`recent_start.rs::tests::get_recent_start_time`) |
| `TestCChainGenesisTimestamp` | ⬜ M8.15 (needs the ava-cchain/reth eth-genesis parse — next wave) |
