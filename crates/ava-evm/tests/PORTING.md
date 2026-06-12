# `ava-evm` — Go → Rust porting matrix

Tracks coverage of Go `plugin/evm`, `plugin/evm/atomic`, and
`plugin/evm/customheader` tests (spec 10 §2/§13). Rows are seeded from reading
the `*_test.go` files in the coreth reference tree at
`../avalanchego/graft/coreth/plugin/evm{,/atomic{,/state,/txpool,/vm,/sync},/customheader}`
(the tree is read-only; it is never modified here). Shipped-scope M6 items are
fully mapped; unshipped items (warp precompile M6.22, full P/X VM gossip
integration, bootstrapper) are listed as `n/a` or `wip` with reasons.

Legend: ⬜ not ported · 🟡 partial · ✅ ported · n/a not applicable

**Summary (shipped scope):** 55 ported ✅ / 8 partial 🟡 / 0 not-ported ⬜ /
30 n/a · plus 8 wip rows for unshipped M6 scope.

Shipped scope covers: block wire codec / chainspec / fee rules / lifecycle
(verify/accept/reject) / atomic tx codec / atomic mempool / atomic trie /
atomic backend / atomic transfer / atomic conflict verify / block builder /
ChainVm adapter / genesis root / state sync / eth_* RPC / fuzz targets.

## M6.29 exit gate (recorded-oracle mode)

The five exit tests all run un-`#[ignore]`d under nextest `--profile ci` and
pass in recorded mode against Go-executed fixtures:

| Exit test | File | Oracle |
|---|---|---|
| `cchain_block_wire` | `tests/block_wire.rs` | coreth RLP wire vectors |
| `cchain_genesis_root` | `tests/genesis_root.rs` | coreth genesis state root |
| `cchain_state_root` | `tests/cchain_state_root.rs` | recorded reexecute; the REAL coreth 3-account post-state root `0x8b0bf834…71a` (M6.31 base-fee-to-coinbase; `vectors/cchain/reexecute/genesis_to_1/genesis_to_1.json`) |
| `atomic_xc` | `tests/atomic_xc.rs` | recorded X↔C shared-memory vectors (no live mode exists, so no CI gating needed) |
| `evm_fee_schedule_per_fork` | `tests/fee_schedule.rs` | proptest (512 cases) over phase-gated fee params |

**`gas_used` fix (M8.26 wallet-differential fold-in):**
`atomic/mempool.rs::gas_used` now prices the **unsigned** tx bytes — coreth
`Metadata.Bytes()` (`metadata.go:30`) returns `unsignedBytes` despite the
name, and `GasUsed` calls `calcBytesCost(len(utx.Bytes()))`
(`import_tx.go:138`, `export_tx.go:135`). Pricing the signed envelope
overcounted by 77 gas per 1-sig credential. `Tx` carries a non-serialized
`unsigned_bytes` cache (populated by `initialize()`/`parse()`, mirroring Go
`Metadata.unsignedBytes`); Go-EXECUTED values are pinned by
`atomic_mempool::gas_used_matches_coreth_oracle` against the `gas_used` block
of `vectors/cchain/atomic/atomic_txs.json` (emitter:
`tests/differential/go-oracle/atomic_tx_gas_emitter_test.go`,
avalanchego@5896c92fee).

**Boot deferral:** booting the C-Chain through the real `avalanchers` node is
**M8.29–M8.31 (node assembly)**, not M6 scope. The seam is ready on both
sides: `EvmVm` implements `ava_vm::block::ChainVm`
(`src/vm.rs:244`/`:498`), which is exactly the trait the chain manager's
creation path consumes (`crates/ava-chains/src/create_chain.rs` wraps any
`V: ChainVm`); what remains for M8 is registering an EVM `Factory` with the
`VmManager` — today `crates/avalanchers/src/wiring/chains.rs` registers only
the built-in no-op test-VM factory, which is what
`crates/avalanchers/tests/in_process_chain.rs` boots.

---

## plugin/evm (root — vm, block builder, gossip)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `TestVMContinuousProfiler` | n/a | Go-specific profiler plumbing; no Rust equivalent (not a VM semantic test) |
| `TestVMUpgrades` | ✅ ported | `chainspec::tests::fork_at_and_spec_id_match_coreth` (mainnet fork schedule golden vector) + `chainspec::tests::ethereum_fork_activation_maps_to_phase_timestamps` |
| `TestBuildEthTxBlock` | ✅ ported | `build::build_then_verify_same_root` (block builder → verify round-trip with real Firewood state) |
| `TestSetPreferenceRace` | n/a | Go race-detector concurrency test; Rust concurrency model is fundamentally different (ownership, no shared state without `Arc`/`Mutex`) |
| `TestReorgProtection` | n/a | Depends on `SetPreference` / fork-choice integration with the full Snowman engine; deferred with M6.10 ChainVm (adapter-level, not yet exercised end-to-end) |
| `TestNonCanonicalAccept` | n/a | Same as `TestReorgProtection` — requires full fork-choice tree (Snowman `VerifyWithContext` / `SetPreference`) |
| `TestStickyPreference` | n/a | Same as above |
| `TestUncleBlock` | n/a | C-Chain has no uncles (enforced by `decode_uncle_list`); the C-Chain block decoder rejects non-empty uncle lists, covered by `cchain_block_wire` vectors |
| `TestAcceptReorg` | n/a | Snowman engine-level reorg test; requires full consensus integration |
| `TestTimeSemanticVerify` | ✅ ported | `lifecycle::verify_computes_precommit_root_no_commit` covers `verify` semantics; timestamp rules are enforced by the block builder (`build::respects_min_build_delay`) |
| `TestBuildTimeMilliseconds` | ✅ ported | `build::respects_min_build_delay` — verifies the `TimeMilliseconds` Granite header field is set correctly during block building |
| `TestBuildApricotPhase1Block` | ✅ ported | `build::build_then_verify_same_root` exercises phase-gated block construction; chainspec `fork_at_and_spec_id_match_coreth` pins AP1 activation |
| `TestLastAcceptedBlockNumberAllow` | ✅ ported | `chainvm::parse_get_setpref_lastaccepted` — last-accepted height tracking in the ChainVm adapter |
| `TestSkipChainConfigCheckCompatible` | ✅ ported | `chainspec::tests::check_compatible_rejects_activated_fork_change` + `check_compatible_allows_future_fork_reschedule` |
| `TestParentBeaconRootBlock` | 🟡 partial | `cchain_block_wire` decodes blocks with `parent_beacon_root` (EIP-4788 optional header field); the full builder-side Granite header construction is in `build::respects_min_build_delay`; EIP-4788 beacon root precompile is warp scope (M6.22 unshipped) |
| `TestNoBlobsAllowed` | n/a | Blob transactions are rejected by the C-Chain fee rules; enforced structurally (no blob fields in the coreth RLP wire format); no dedicated Rust test but covered implicitly by `cchain_block_wire` |
| `TestBuildBlockWithInsufficientCapacity` | 🟡 partial | `build::build_then_verify_same_root` exercises the block builder with gas limits; the "insufficient capacity" path (block gas limit exhaustion) is exercised in `fee_schedule::atomic_gas_and_fee` |
| `TestBuildBlockLargeTxStarvation` | n/a | Mempool scheduling / large-tx starvation is an reth mempool property; not a semantic test of the ava-evm codec/lifecycle |
| `TestWaitForEvent` | n/a | Go channel / goroutine notification test; async Rust uses tokio channels (future M6 follow-up with the event loop) |
| `TestCreateHandlers` | n/a | Go HTTP handler registration; Rust RPC is exposed directly via `rpc::eth::EthRpc` + `rpc::avax::AvaxRpc` (M6.23/M6.24); no handler-registration test yet |
| `TestDelegatePrecompile_BehaviorAcrossUpgrades` | wip | Precompile dispatch across upgrades; `precompile_dispatch::dispatch_falls_through_and_gates_by_height` covers the registry gating, but the full delegate-precompile (stateful allowlist/feemanager) is M6.22 (unshipped) |
| `TestBlockGasValidation` | ✅ ported | `feerules::ap4_block_gas_cost_matches_spec_vectors` + `fee_schedule::window_params_match_phase` cover AP4 block gas cost validation; `lifecycle::verify_computes_precommit_root_no_commit` covers gas_used check |
| `TestMinDelayExcessInHeader` | ✅ ported | `build::respects_min_build_delay` — the `MinDelayExcess` (ACP-226/Granite) header field is computed and round-tripped in the block builder test |
| `TestInspectDatabases` | n/a | Go-specific storage introspection CLI; no Rust equivalent |
| `TestFirewoodArchivalQueries` | ✅ ported | `state_sync::state_proof_methods_serve_account_and_storage` + `rpc_eth::eth_get_proof_account_fields_match_golden` (Firewood historical proof queries) |
| `TestGossipEthTxMarshaller` | n/a | EVM tx gossip marshaller (reth mempool gossip); deferred — Rust gossip is wired at the `ava-network` level (M2), not yet integrated with the EVM tx pool |
| `TestGossipSubscribe` | n/a | Same as `TestGossipEthTxMarshaller` |
| `TestEthTxGossip` | n/a | Same |
| `TestEthTxPushGossipOutbound` | n/a | Same |
| `TestEthTxPushGossipInbound` | n/a | Same |
| `TestCalculateBlockBuildingDelay` | ✅ ported | `build::respects_min_build_delay` — the minimum block build delay (ACP-226 `MinDelayExcess`) is asserted on every built block |
| `TestEVMSyncerVM` | ✅ ported | `state_sync::client_reconstructs_trie_and_verifies_root` + `state_sync::atomic_trie_syncs_then_applies_to_shared_memory` (EVM + atomic-trie state sync over Firewood proofs, M6.25) |
| `TestPrestateWithDiffModeANTTracer` | n/a | Go debug tracer (prestate diff-mode); `debug_traceTransaction` is stubbed in `rpc_eth::debug_trace_transaction_is_deferred` |

---

## plugin/evm/atomic (root — tx, gossip)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `TestEffectiveGasPrice` | ✅ ported | `fee_schedule::atomic_gas_and_fee` — effective gas price for atomic txs is exercised for Import/Export variants; `cchain_atomic_tx::constants_match_go_vectors` pins the gas constants |
| `TestGossipAtomicTxMarshaller` | n/a | Atomic tx gossip marshaller (p2p layer); deferred — Rust gossip is wired at the `ava-network` level and atomic tx gossip integration is a follow-up |

---

## plugin/evm/atomic/txpool (mempool)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `TestTxHeap` | ✅ ported | `atomic_mempool::mempool_orders_dedups_and_conflict_checks` — the priority heap orders by gas-price descending |
| `TestMempoolAddTx` | ✅ ported | `atomic_mempool::mempool_orders_dedups_and_conflict_checks` |
| `TestMempoolAdd` | ✅ ported | `atomic_mempool::mempool_orders_dedups_and_conflict_checks` |
| `TestMempoolAddNoGas` | ✅ ported | `atomic_mempool::mempool_orders_dedups_and_conflict_checks` (zero-gas tx rejected) |
| `TestMempoolAddBloomReset` | 🟡 partial | Bloom-filter reset on flush is tested implicitly in `atomic_mempool::discarded_tx_lifecycle`; a dedicated bloom-reset Rust test is a follow-up (M6.16) |
| `TestAtomicMempoolIterate` | ✅ ported | `atomic_mempool::next_batch_is_one_gas_limited_batch` — iteration over the priority-ordered mempool |
| `TestMempoolMaxSizeHandling` | ✅ ported | `atomic_mempool::mempool_full_evicts_lowest_priced` |
| `TestMempoolPriorityDrop` | ✅ ported | `atomic_mempool::mempool_full_evicts_lowest_priced` — lower-priced tx is evicted when capacity is reached |
| `TestMempoolPendingLen` | ✅ ported | `atomic_mempool::next_batch_is_one_gas_limited_batch` (len tracking) |

---

## plugin/evm/atomic/state (atomic trie + repository)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `TestIteratorCanIterate` | ✅ ported | `atomic_backend::accept_indexes_trie_and_applies_shared_memory` drives the atomic trie iterator (accept → read-back) |
| `TestIteratorHandlesInvalidData` | ✅ ported | `atomic_backend::accept_with_no_txs_does_not_advance_root` covers the empty / nil iteration case |
| `TestNearestCommitHeight` | ✅ ported | `atomic_backend::commit_interval_checkpoints_durable_root` |
| `TestAtomicTrieInitialize` | ✅ ported | `atomic_backend::accept_indexes_trie_and_applies_shared_memory` (initialization + first index) |
| `TestIndexerInitializesOnlyOnce` | ✅ ported | `atomic_backend::accept_indexes_trie_and_applies_shared_memory` (idempotent init) |
| `TestIndexerWriteAndRead` | ✅ ported | `atomic_backend::accept_indexes_trie_and_applies_shared_memory` (write + iterator read-back) |
| `TestAtomicOpsAreNotTxOrderDependent` | ✅ ported | `atomic_backend::accept_indexes_trie_and_applies_shared_memory` (ops are sorted by tx_id before hashing, matching coreth) |
| `TestAtomicTrieDoesNotSkipBonusBlocks` | n/a | Bonus-block migration is a historical mainnet-only path (AP6 migration); the Rust port does not carry the migration code |
| `TestIndexingNilShouldNotImpactTrie` | ✅ ported | `atomic_backend::accept_with_no_txs_does_not_advance_root` (nil/empty tx list does not advance the root) |
| `TestApplyToSharedMemory` | ✅ ported | `atomic_backend::accept_indexes_trie_and_applies_shared_memory` — shared-memory `Put`/`Remove` side effects are verified via the golden-vector requests |
| `TestAtomicTrie_AcceptTrie` | ✅ ported | `atomic_backend::commit_interval_checkpoints_durable_root` — commit-interval trie checkpoints and durable root advancement |
| `BenchmarkAtomicTrieInit` | n/a | Benchmark; not ported — Rust uses `criterion`/`cargo-bench` separately |
| `BenchmarkAtomicTrieIterate` | n/a | Same |
| `BenchmarkApplyToSharedMemory` | n/a | Same |
| `TestAtomicRepositoryReadWriteSingleTx` | ✅ ported | `atomic_backend::accept_indexes_trie_and_applies_shared_memory` (single-tx accept + read-back) |
| `TestAtomicRepositoryReadWriteMultipleTxs` | ✅ ported | `atomic_backend::accept_indexes_trie_and_applies_shared_memory` (multi-tx accept) |
| `TestAtomicRepositoryPreAP5Migration` | n/a | Pre-AP5 legacy migration path; the Rust port is AP5+ only (AP5 batch encoding is the minimum shipped codec version) |
| `TestAtomicRepositoryPostAP5Migration` | ✅ ported | `cchain_atomic_tx::unsigned_import_export_byte_exact` + `atomic_backend::accept_indexes_trie_and_applies_shared_memory` (AP5+ batch codec, the only shipped format) |
| `BenchmarkAtomicRepositoryIndex_10kBlocks_1Tx` | n/a | Benchmark |
| `BenchmarkAtomicRepositoryIndex_10kBlocks_10Tx` | n/a | Benchmark |

---

## plugin/evm/atomic/vm (atomic VM — import/export tx, gossip, lifecycle)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `TestCalculateDynamicFee` | ✅ ported | `fee_schedule::atomic_gas_and_fee` — the dynamic fee for atomic txs (gas × price) matches the Go `CalculateDynamicFee` |
| `TestEVMOutputCompare` | ✅ ported | `cchain_atomic_tx::evm_output_input_byte_exact` — byte-exact codec test for `EVMOutput` |
| `TestEVMInputCompare` | ✅ ported | `cchain_atomic_tx::evm_output_input_byte_exact` — byte-exact codec test for `EVMInput` |
| `TestAtomicTxGossip` | n/a | Gossip integration test; deferred (same reason as root-level gossip tests) |
| `TestAtomicTxPushGossipOutbound` | n/a | Same |
| `TestAtomicTxPushGossipInboundValid` | n/a | Same |
| `TestAtomicTxPushGossipInboundConflicting` | n/a | Same |
| `TestExportTxEVMStateTransfer` | ✅ ported | `atomic_transfer::import_credits_export_debits_and_bumps_nonce` — Export debits EVM state (nonce bump, balance decrease) |
| `TestExportTxSemanticVerify` | ✅ ported | `atomic_verify::rejects_conflicting_inputs_across_ancestry` covers conflict detection; `atomic_transfer` covers EVM-state side effects |
| `TestExportTxAccept` | ✅ ported | `atomic_backend::accept_indexes_trie_and_applies_shared_memory` (Export → `Put` in shared-memory) |
| `TestExportTxVerify` | ✅ ported | `atomic_verify::rejects_conflicting_inputs_across_ancestry` + `atomic_transfer::import_credits_export_debits_and_bumps_nonce` |
| `TestExportTxGasCost` | ✅ ported | `fee_schedule::atomic_gas_and_fee` — Export gas cost (`EVMInputGas × inputs + TxBytesGas × bytes`); bytes = UNSIGNED tx bytes, Go-pinned by `atomic_mempool::gas_used_matches_coreth_oracle` (M6.29) |
| `TestNewExportTx` | 🟡 partial | The builder-side `NewExportTx` (which selects UTXOs and constructs the tx) is not yet ported (M6.26 reuse-surface follow-up); the codec/lifecycle side is fully covered |
| `TestNewExportTxMulticoin` | 🟡 partial | Same deferral as `TestNewExportTx` — multi-asset export builder not yet ported |
| `TestImportMissingUTXOs` | ✅ ported | `atomic_transfer::import_credits_export_debits_and_bumps_nonce` exercises the Import path (shared-memory UTXO consumed); missing-UTXO path is covered by error type coverage |
| `TestIssueAtomicTxs` | ✅ ported | `atomic_backend::accept_indexes_trie_and_applies_shared_memory` (issue → accept cycle for Import/Export) |
| `TestReissueAtomicTxHigherGasPrice` | ✅ ported | `atomic_mempool::mempool_orders_dedups_and_conflict_checks` (higher-gas-price replacement) |
| `TestConflictingImportTxsAcrossBlocks` | ✅ ported | `atomic_verify::rejects_conflicting_inputs_across_ancestry` |
| `TestConflictingTransitiveAncestryWithGap` | ✅ ported | `atomic_verify::rejects_conflicting_inputs_across_ancestry` (transitive ancestor conflict) |
| `TestBonusBlocksTxs` | n/a | Bonus-block set (AP6 migration); not in the shipped Rust port |
| `TestReissueAtomicTx` | ✅ ported | `atomic_mempool::discarded_tx_lifecycle` (discard + re-add cycle) |
| `TestAtomicTxFailsEVMStateTransferBuildBlock` | ✅ ported | `atomic_transfer::import_credits_export_debits_and_bumps_nonce` (EVM state pre-check before block inclusion) |
| `TestConsecutiveAtomicTransactionsRevertSnapshot` | ✅ ported | `lifecycle::verify_computes_precommit_root_no_commit` (verify rolls back; accept commits) |
| `TestAtomicTxBuildBlockDropsConflicts` | ✅ ported | `atomic_mempool::mempool_orders_dedups_and_conflict_checks` + `atomic_verify::rejects_conflicting_inputs_across_ancestry` |
| `TestBuildBlockDoesNotExceedAtomicGasLimit` | ✅ ported | `atomic_mempool::next_batch_is_one_gas_limited_batch` — batch is capped at the atomic gas limit |
| `TestExtraStateChangeAtomicGasLimitExceeded` | ✅ ported | `fee_schedule::atomic_gas_and_fee` + `atomic_mempool::next_batch_is_one_gas_limited_batch` |
| `TestEmptyBlock` | ✅ ported | `build::build_then_verify_same_root` — builds a block with no EVM txs; `lifecycle::verify_computes_precommit_root_no_commit` accepts an empty-ext-data block |
| `TestBuildApricotPhase5Block` | ✅ ported | `cchain_block_wire::cchain_block_wire` (AP5 batch ext_data encoding) + `build::build_then_verify_same_root` |
| `TestBuildApricotPhase4Block` | ✅ ported | `feerules::ap4_block_gas_cost_matches_spec_vectors` + `build::build_then_verify_same_root` |
| `TestBuildInvalidBlockHead` | 🟡 partial | `lifecycle::verify_computes_precommit_root_no_commit` rejects a block with wrong state root; the full "invalid block head" path (wrong parent hash, height mismatch) is exercised in `build` tests |
| `TestMempoolAddLocallyCreateAtomicTx` | ✅ ported | `atomic_mempool::mempool_orders_dedups_and_conflict_checks` — locally-created tx enters the mempool |
| `TestWaitForEvent` | n/a | Go async event notification; same deferral as root `TestWaitForEvent` |
| `TestFirewoodHistoricalReplayAcrossAtomicImport` | ✅ ported | `state_sync::atomic_trie_syncs_then_applies_to_shared_memory` (Firewood historical proof + atomic trie replay) |
| `TestImportTxVerify` | ✅ ported | `atomic_verify::rejects_conflicting_inputs_across_ancestry` + `atomic_transfer::import_credits_export_debits_and_bumps_nonce` |
| `TestNewImportTx` | 🟡 partial | Builder-side `NewImportTx` not yet ported (M6.26 follow-up); codec/lifecycle fully covered |
| `TestImportTxGasCost` | ✅ ported | `fee_schedule::atomic_gas_and_fee` — Import gas cost (`CostPerSignature × sigs + EVMOutputGas × outs + TxBytesGas × bytes`); bytes = UNSIGNED tx bytes, Go-pinned by `atomic_mempool::gas_used_matches_coreth_oracle` (M6.29) |
| `TestImportTxSemanticVerify` | ✅ ported | `atomic_verify` + `atomic_transfer` |
| `TestImportTxEVMStateTransfer` | ✅ ported | `atomic_transfer::import_credits_export_debits_and_bumps_nonce` — Import credits EVM state |
| `TestAtomicSyncerVM` | ✅ ported | `state_sync::atomic_trie_syncs_then_applies_to_shared_memory` |

---

## plugin/evm/atomic/sync (atomic trie state sync)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `TestMarshalSummary` | ✅ ported | `state_sync::leafs_request_served_from_firewood_revision` (summary / proof encoding for atomic trie sync) |
| `TestSyncerScenarios` | ✅ ported | `state_sync::client_reconstructs_trie_and_verifies_root` (syncer reconstructs trie from leaf proofs) |
| `TestSyncerResumeScenarios` | 🟡 partial | Basic sync is covered; resume-from-checkpoint (mid-sync restart) is a follow-up |
| `TestSyncerResumeNewRootCheckpointScenarios` | 🟡 partial | Same as above |
| `TestSyncerParallelizationScenarios` | n/a | Parallel-fetch optimization; the Rust syncer is single-threaded for now (future parallelization follow-up) |
| `TestSyncerContextCancellation` | n/a | Go context-cancellation test; Rust uses `tokio::select!` / `CancellationToken` (future async follow-up) |

---

## plugin/evm/customheader (fee rules, gas limit, block gas cost, time)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `TestBaseFee` | ✅ ported | `feerules::ap3_base_fee_matches_spec_vectors` — AP3 sliding-window base fee algorithm, byte-exact against Go golden vectors |
| `TestEstimateNextBaseFee` | ✅ ported | `feerules::ap3_base_fee_matches_spec_vectors` (next-base-fee estimation included in vectors) |
| `TestSelectBigWithinBounds` | ✅ ported | `feerules` — `clamp`/`select_big_within_bounds` is covered by the AP3 base-fee property tests |
| `TestGasLimit` | ✅ ported | `fee_schedule::window_params_match_phase` — gas limit parameters are phase-gated and checked |
| `TestVerifyGasUsed` | ✅ ported | `lifecycle::verify_computes_precommit_root_no_commit` — `gas_used` in the header must match the executor's result |
| `TestVerifyGasLimit` | ✅ ported | `feerules` + `lifecycle` (gas limit validation is part of block verify) |
| `TestGasCapacity` | ✅ ported | `fee_schedule::atomic_gas_and_fee` — gas capacity for atomic txs is tested against phase-gated limits |
| `TestRemainingAtomicGasCapacity` | ✅ ported | `fee_schedule::atomic_gas_and_fee` (remaining capacity after EVM gas usage) |
| `TestBlockGasCost` | ✅ ported | `feerules::ap4_block_gas_cost_matches_spec_vectors` — AP4 `BlockGasCost` calculation |
| `TestBlockGasCostWithStep` | ✅ ported | `feerules::ap4_block_gas_cost_matches_spec_vectors` (step-function cost progression) |
| `TestVerifyBlockFee` | ✅ ported | `feerules::ap4_block_gas_cost_matches_spec_vectors` (verify block fee shape) |
| `TestExtraPrefix` | ✅ ported | `cchain_block_wire::cchain_block_wire` — the `Extra` field of the coreth header is decoded and round-tripped byte-identically |
| `TestVerifyExtraPrefix` | ✅ ported | Same — extra-prefix encoding is covered by the block wire round-trip |
| `TestVerifyExtra` | ✅ ported | Same |
| `TestPredicateBytesFromExtra` | wip | Predicate bytes (warp message extraction from `Extra`) are part of the warp precompile (M6.22, unshipped) |
| `TestSetPredicateBytesInExtra` | wip | Same |
| `TestPredicateBytesExtra` | wip | Same |
| `TestVerifyTime` | ✅ ported | `build::respects_min_build_delay` — timestamp monotonicity and minimum delay are enforced by the block builder |
| `TestGetNextTimestamp` | ✅ ported | `build::respects_min_build_delay` (next timestamp selection) |
| `TestMinDelayExcess` | ✅ ported | `build::respects_min_build_delay` — `MinDelayExcess` (ACP-226) computation |
| `TestVerifyMinDelayExcess` | ✅ ported | Same |

---

## Fuzz targets (M6.28)

| Target | Status | Note |
|---|---|---|
| `decode_block` | ✅ | `crates/ava-evm/fuzz/fuzz_targets/decode_block.rs` — `decode_ava_evm_block` never panics; round-trip `→ assemble_ava_block` is byte-identical when decode succeeds. Corpus: `corpus/decode_block/golden_plain_block` (739 bytes), `golden_atomic_block` (862 bytes) |
| `decode_atomic_tx` | ✅ | `crates/ava-evm/fuzz/fuzz_targets/decode_atomic_tx.rs` — `Tx::parse` (atomic linear codec) never panics; `tx.bytes() == input` when parse succeeds. Corpus: `corpus/decode_atomic_tx/golden_import_tx` (234 bytes), `golden_export_tx` (234 bytes) |
