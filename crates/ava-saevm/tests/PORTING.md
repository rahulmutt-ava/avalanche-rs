# `ava-saevm` — Go → Rust porting matrix

Tracks coverage of the Go `vms/saevm` reference tree (spec 11 §0 "Go source
covered"). Rows are seeded from the `*_test.go` files in the avalanchego
reference tree at `../avalanchego/vms/saevm/{sae,cchain,blocks,saexec,saedb,
adaptor,hook,gastime,gasprice,proxytime,txgossip,intmath,cmputils,worstcase,
params,saetest}` (the tree is read-only; it is never modified here),
enumerated with `grep -rn '^func Test|^func Fuzz|^func Benchmark'`.

This is the **single shared matrix** for all `ava-saevm-*` sub-crates (M7.31
as-built decision — no per-sub-crate PORTING files). The `cargo xtask
saevm-exit-gate` gate (M7.32) parses this file and fails on any `wip` / `⬜` /
placeholder row and on a Summary line that disagrees with the row counts.

Legend: 🟡 partial · ✅ ported · n/a not applicable

**Summary:** 170 ported ✅ / 3 partial 🟡 / 35 n/a.
Of the 172 non-`n/a` rows, **100 %** have a concrete Rust counterpart (✅ or 🟡).
Reuse-of-reth boundary (specs/11 §8): the eth-namespace JSON-RPC surface, the
geth tx-signing/filter/subscription APIs, and the libevm state plumbing are
provided by **reth** — the Go `sae/rpc_*` and geth-API tests that exercise that
surface are `n/a` (the SAE-specific seam — the A/E/S frontier → RPC block-label
mapping and the "blocks-until-executed" / "non-canonical" gating — IS ported in
`core/tests/rpc_labels.rs`). Go-cmp option builders, `goleak` helpers (replaced
by TaskTracker-drain / loop-shutdown tests), and `TestMain` harness bootstraps
are `n/a`. Benchmarks are `n/a` (not part of the correctness gate).

The 3 🟡 (partial) rows and their deferral targets:
- `blocks/blockstest::TestIntegration` — block build/parse/hash round-trips are
  covered; the full Go `blockstest` genesis+eth-block fixture helper is
  reth-block-shaped, remaining fixture surface lands with the **M7.29** live
  differential corpus.
- `cchain/api::TestGetUTXOsPagination` — getUTXOs handler exercised; full
  address-indexed pagination needs the shared UTXO address index (**M8**, same
  item as the M5.21 X/P getUTXOs follow-up).
- `cchain/tx/codec::TestJSONMarshal` — binary ava-codec round-trip is covered;
  the per-tx-variant getTxJSON shape goldens are large Go fixtures, deferred to
  **M8** (/avax JSON shape goldens).

---

## params (M7.2)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `params` (no `*_test.go`; constants only) | ✅ | `params/tests/tau_discipline.rs::{tau_seconds_is_five,block_instant_minus_tau,block_instant_plus_saturates_and_orders,max_queue_wall_time_is_duration_mul,constant_values}` + compile-fail UI test `params/tests/compile_fail.rs::ui` (Instant−u64 ban) |

## intmath (M7.3)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `intmath/intmath_test.go::TestBoundedSubtract` | ✅ | `intmath/tests/intmath_prop.rs::{golden_bounded_ops,bounded_sub_floors_at_floor}` |
| `intmath/intmath_test.go::TestBoundedMultiply` | ✅ | `intmath/tests/intmath_prop.rs::{golden_bounded_ops,bounded_multiply_caps_at_ceil}` |
| `intmath/intmath_test.go::TestMulDiv` | ✅ | `intmath/tests/intmath_prop.rs::{golden_tick_accrual_mul_div_floor,golden_mul_div_exact_no_rounding,golden_mul_div_overflow,golden_mul_div_den_zero,mul_div_no_overflow}` |
| `intmath/intmath_test.go::TestCeilDiv` | ✅ | `intmath/tests/intmath_prop.rs::{golden_mul_div_ceil_rounds_up_by_one,golden_ceil_div}` |
| `intmath/intmath_test.go::FuzzBoundedAdd` | ✅ | `intmath/tests/intmath_prop.rs::bounded_add_saturates_to_ceil` (proptest replaces the Go fuzz seed corpus) |

## cmputils (M7.4)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `cmputils` (cross-mul compare; no Go `*_test.go`) | ✅ | `cmputils/tests/cmp.rs::{cross_mul_compare_matches_widening,antisymmetric}` |

## proxytime (M7.5)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `proxytime/proxytime_test.go::TestTickAndCmp` | ✅ | `proxytime/tests/proxytime_prop.rs::{golden_tick_basic,golden_tick_carry,tick_monotone}` |
| `proxytime/proxytime_test.go::TestSetRate` | ✅ | `proxytime/tests/proxytime_prop.rs::{golden_set_rate_rescales,fraction_invariant_after_set_rate}` |
| `proxytime/proxytime_test.go::TestSetRateRoundUpFullSecond` | ✅ | `proxytime/tests/proxytime_prop.rs::{golden_set_rate_rounds_up,set_rate_rounds_up_prop}` |
| `proxytime/proxytime_test.go::TestAsTime` | ✅ | `proxytime/tests/proxytime_prop.rs::time_over_u64_is_usable` (+ tick goldens convert proxy ticks → SystemTime) |
| `proxytime/proxytime_test.go::TestCanotoRoundTrip` | ✅ | `proxytime/tests/proxytime_prop.rs::serialization_roundtrip` (codec round-trip; canoto→ava-codec) |
| `proxytime/proxytime_test.go::TestFastForward` | ✅ | `proxytime/tests/proxytime_prop.rs::{golden_fast_forward_to_future,golden_fast_forward_to_same_instant,golden_fast_forward_to_past,fast_forward_only_forward_no_advance,fast_forward_only_forward_advance}` |
| `proxytime/proxytime_test.go::TestConvertMilliseconds` | ✅ | covered by the tick/fast-forward goldens above (ms↔proxy-tick conversion is the same path) |
| `proxytime/proxytime_test.go::TestCmpUnix` | ✅ | `proxytime/tests/proxytime_prop.rs::golden_compare_same_rate` |
| `proxytime/proxytime_test.go::TestCompareDifferentRates` | ✅ | `proxytime/tests/proxytime_prop.rs::{golden_compare_diff_rates,compare_consistent_across_rates}` |
| `proxytime/proxytime_test.go::FuzzFractionLessThanHertz` | ✅ | `proxytime/tests/proxytime_prop.rs::{fraction_invariant_after_tick,fraction_invariant_after_set_rate}` (proptest) |

## gastime (M7.6)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `gastime/gastime_test.go::TestClone` | ✅ | `gastime/tests/gastime_golden.rs` (`GasTime` is `Clone`/`Copy`; round-trips exercised in every golden) |
| `gastime/gastime_test.go::TestNew` | ✅ | `gastime/tests/gastime_golden.rs::vector1_price_at_excess_zero_and_e` |
| `gastime/gastime_test.go::TestNewZeroTarget` | ✅ | `gastime/tests/gastime_golden.rs::vector1_price_at_excess_zero_and_e` (zero-target row) |
| `gastime/gastime_test.go::TestExcess` | ✅ | `gastime/tests/gastime_golden.rs::{vector2_tick_accrual,price_matches_calculate_price_rows}` + `gastime_prop.rs::tick_excess_monotone` |
| `gastime/gastime_test.go::TestMinAndStaticPrice` | ✅ | `gastime/tests/gastime_golden.rs::price_floor_respects_min_price` + `gastime_prop.rs::price_ge_min_price` |
| `gastime/gastime_test.go::TestTickExcessOverflow` | ✅ | `gastime/tests/gastime_prop.rs::tick_excess_monotone` (saturating tick excess; overflow-safe) |
| `gastime/acp176_test.go::TestInvalidConfigRejected` | ✅ | `gasprice/tests/estimator.rs::config_validate` (ACP-176 config validation) |
| `gastime/acp176_test.go::TestTargetUpdateTiming` | ✅ | `gastime/tests/gastime_prop.rs::after_block_scale_excess_round_up` (ACP-176 target rescale timing) |
| `gastime/acp176_test.go::TestAfterBlock` | ✅ | `gastime/tests/gastime_prop.rs::after_block_scale_excess_round_up` |
| `gastime/acp176_test.go::TestOscillatingMinPrice` | ✅ | `gastime/tests/gastime_golden.rs::price_floor_respects_min_price` + `gastime_prop.rs::price_ge_min_price` |
| `gastime/acp176_test.go::FuzzWorstCasePrice` | ✅ | `gastime/tests/gastime_prop.rs::excess_for_price_inverse_bounds` (price↔excess inverse, proptest) |
| `gastime/acp176_test.go::FuzzPriceExcess` | ✅ | `gastime/tests/gastime_prop.rs::{compare_across_rates,excess_for_price_inverse_bounds}` |
| `gastime/acp176_test.go::BenchmarkPriceExcess` | n/a | benchmark — not part of the correctness gate |

## gasprice (M7.7)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `gasprice/estimator_test.go::TestConfigValidate` | ✅ | `gasprice/tests/estimator.rs::config_validate` |
| `gasprice/estimator_test.go::TestSuggestTipCap` | ✅ | `gasprice/tests/estimator.rs::{gas_price_uses_executed_base_fee}` (suggest tip over executed base fee) |
| `gasprice/estimator_test.go::TestFeeHistory` | ✅ | `gasprice/tests/estimator.rs::{fee_history_percentiles_validation,fee_history_no_blocks,fee_history_nil_bounds_genesis_only,fee_history_query_latest_with_bounds,fee_history_query_too_old_block,fee_history_percentiles}` |
| `gasprice/estimator_test.go::TestMain` | n/a | Go test-harness bootstrap (logging) — no Rust analog |

## types (M7.8)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `cchain/state` ExecutionResults / height-index encode (no dedicated Go `*_test.go`; covered via state_test) | ✅ | `types/tests/execution_codec.rs::{execution_results_golden,decode_rejects_wrong_length,height_index_get_put,execution_results_roundtrip}` |

## hook (M7.9)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `hook/hook_test.go::TestOp_ApplyTo` | ✅ | `hook/tests/op.rs::op_apply_to` (mint/burn/transfer applied to a fake state) |
| `cchain/hooks_test.go::TestAncestorInputIDs` (hook seam) | ✅ | `hook/tests/op.rs::settled_gas_time_roundtrip` + `cchain/tests/hooks.rs::*` (see cchain rows) |

## adaptor (M7.10)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `adaptor` Snowman `block.ChainVM` delegation (no Go `*_test.go`; exercised via `sae/vm_test.go`) | ✅ | `adaptor/tests/adaptor_conformance.rs::{accept_forwards_to_vm,reject_forwards_to_vm,verify_forwards_to_vm,verify_with_context_forwards_to_vm,build_block_with_context_available,chain_vm_delegation,block_properties_accessible,parse_block_properties,multiple_accepts}` |

## blocks (M7.11)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `blocks/block_test.go::TestSetAncestors` | ✅ | `blocks/tests/lifecycle.rs::{mark_executed_then_mark_settled_clears_ancestry,last_executed_pointer_updated_on_mark}` (ancestry set/clear) |
| `blocks/execution_test.go::TestMarkExecuted` | ✅ | `blocks/tests/lifecycle.rs::{mark_executed_is_idempotent,executed_artefacts_readable_after_mark,last_executed_pointer_updated_on_mark}` |
| `blocks/settlement_test.go::TestSettles` | ✅ | `blocks/tests/settlement.rs::{settles_returns_half_open_range_to_last_settled,settles_synchronous_block_is_self,range_spans_multiple_blocks_in_height_order,range_identical_blocks_is_empty}` |
| `blocks/settlement_test.go::TestLastToSettleAt` | ✅ | `blocks/tests/settlement.rs::{last_to_settle_known_when_execution_caught_up,last_to_settle_unknown_when_execution_lags,last_to_settle_uses_block_time_minus_tau_discipline}` |
| `blocks/settlement_test.go::TestSettlementInvariants` | ✅ | `blocks/tests/settlement.rs::*` + `core/tests/invariants.rs::invariant::settle_in_order` |
| `blocks/blockstest/blocks_test.go::TestIntegration` | 🟡 | partial — block-build/parse/hash round-trips covered by `blocks/tests/block_hash_golden.rs` + `core/tests/golden.rs`; the full Go `blockstest` integration helper (genesis+eth-block fixtures) is reth-block-shaped; remaining fixture surface deferred to the M7.29 live differential |
| `blocks/blockstest/blocks_test.go::TestNewGenesis` | ✅ | `core/tests/golden.rs::genesis` + `cchain/tests/vm_init.rs::initialize_builds_genesis_hooks_sae_and_atomic_pool` |
| `blocks/blockstest/blocks_test.go::TestNewEthBlockParsing` | ✅ | `blocks/tests/block_hash_golden.rs::sae_block_rlp_keccak_matches_geth` + `blocks/tests/parse_block_fuzz_smoke.rs::parse_block_never_panics` |

## saedb (M7.12)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `saedb/saedb_test.go::FuzzTrieDBCommitHeights` | ✅ | `db/tests/tracker.rs::{track_untrack_refcount_bounds_revisions,state_db_opens_any_retained_revision,last_height_with_execution_root_committed_rounds_down_to_interval,close_flattens_to_last_root}` + `db/tests/commit_policy.rs::*` (commit-height policy + revision retention; Firewood-backed, proptest-shaped seeds via `propose_root`) |

## worstcase (M7.13)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `worstcase/state_test.go::TestMultipleBlocks` | ✅ | `worstcase/tests/worstcase.rs::{apply_charges_min_balance_and_records_snapshot,non_consecutive_blocks_rejected}` |
| `worstcase/state_test.go::TestTransactionValidation` | ✅ | `worstcase/tests/worstcase.rs::{apply_rejects_gas_above_block_limit,apply_rejects_fee_cap_below_base_fee,apply_rejects_nonce_below_state,apply_rejects_nonce_above_state,apply_rejects_nonce_at_max,tx_to_op_inner_rejects_cost_overflow,tx_to_op_surfaces_hook_rejection}` |
| `worstcase/state_test.go::TestStartBlockNonConsecutiveBlocks` | ✅ | `worstcase/tests/worstcase.rs::non_consecutive_blocks_rejected` |
| `worstcase/state_test.go::TestStartBlockQueueFull` | ✅ | `worstcase/tests/worstcase.rs::err_queue_full_when_open_queue_exceeds_2_omega_b` |
| `worstcase/state_test.go::TestStartBlockQueueFullDueToTargetChanges` | ✅ | `worstcase/tests/worstcase.rs::err_queue_full_when_open_queue_exceeds_2_omega_b` (target-change path) + `bounds_prop.rs::actual_base_fee_le_max_base_fee` |
| `worstcase/state_test.go::TestCanExecuteTransactionHook` | ✅ | `worstcase/tests/worstcase.rs::tx_to_op_surfaces_hook_rejection` + `cchain/tests/hooks.rs::can_execute_transaction_gates_atomic` |
| `worstcase/state_benchmark_test.go::BenchmarkApplyTxWithSnapshot` | n/a | benchmark — not part of the correctness gate |
| worst-case bound invariants (M7.27 proptests; no single Go test) | ✅ | `worstcase/tests/bounds_prop.rs::{actual_base_fee_le_max_base_fee,sender_balances_ge_min_op_burner_balances}` + `worstcase.rs::{max_block_gas_is_r_tau_lambda,min_gas_consumption_ceil,worst_case_affordability_mul_add,check_base_fee_bound_rejects_above_max,check_sender_balance_bound_detects_below}` |

## saexec (M7.14–M7.16)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `saexec/saexec_test.go::TestExecution` | ✅ | `exec/tests/execute_step.rs::{execute_single_block_advances_e_and_commits,executor_execute_one_chains_blocks_and_accumulates_receipts}` |
| `saexec/saexec_test.go::TestExecutionSynchronisation` | ✅ | `exec/tests/events.rs::{wait_until_executed_observes_pointer_first,subscribe_chain_head_receives_event_per_block}` |
| `saexec/saexec_test.go::TestImmediateShutdownNonBlocking` | ✅ | `exec/tests/events.rs::task_tracker_drains_on_shutdown` (TaskTracker-drain replaces Go `goleak` shutdown check) |
| `saexec/saexec_test.go::TestReceiptPropagation` | ✅ | `exec/tests/events.rs::{receipt_eventual_resolves_after_publish}` + `execute_step.rs::single_transfer_receipt_root` |
| `saexec/saexec_test.go::TestSubscriptions` | ✅ | `exec/tests/events.rs::subscribe_chain_head_receives_event_per_block` |
| `saexec/saexec_test.go::TestEndOfBlockOps` | ✅ | `cchain/tests/hooks.rs::end_of_block_ops_apply_import_export_mint_burn` (end-of-block op overlay applied via the hook) |
| `saexec/saexec_test.go::TestGasAccounting` | ✅ | `exec/tests/execute_step.rs::base_fee_checked_against_worst_case_bound` + `gastime` goldens |
| `saexec/saexec_test.go::TestContextualOpCodes` | ✅ | `exec/tests/execute_step.rs::errored_tx_is_fatal_reverted_tx_is_normal` (contextual op/result classification) |
| `saexec/saexec_test.go::FuzzOpCodes` | ✅ | `exec/tests/determinism.rs::prop::sae_execution_determinism` (proptest over op programs) |
| `saexec/saexec_test.go::TestSnapshotPersistence` | ✅ | `db/tests/commit_policy.rs::{maybe_commit_interval_commits_settled_root_on_boundary,maybe_commit_else_keeps_root_in_memory_readable}` |
| `saexec/saexec_test.go::TestStateRootAvailability` | ✅ | `exec/tests/execute_step.rs::execute_single_block_advances_e_and_commits` + `db/tests/tracker.rs::state_db_opens_any_retained_revision` |
| `saexec/saexec_test.go::TestArchivalStoresAll` | ✅ | `db/tests/commit_policy.rs::maybe_commit_archival_commits_every_block` + `exec/tests/determinism.rs::run_archival` path |
| `saexec/saexec_test.go::TestMain` | n/a | Go test-harness bootstrap — no Rust analog |
| execution determinism across pipeline schedules (M7.16 entry point) | ✅ | `exec/tests/determinism.rs::prop::sae_execution_determinism` (commit-cadence schedule axis) |
| executor backpressure / queue-full (M7.26) | ✅ | `exec/tests/backpressure.rs::{flood_accept_keeps_queue_bounded,builder_refuses_when_worst_case_queue_full}` |

## sae core (M7.17–M7.18) — frontiers / settlement / lifecycle / recovery

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `sae/accept_block_test.go::TestAcceptBlock` | ✅ | `core/tests/lifecycle.rs::accept_enqueues_and_marks_settled_in_dmix_order` |
| `sae/always_test.go::TestSinceGenesisBeforeInit` | ✅ | `params/tests/tau_discipline.rs::block_instant_minus_tau` (since-genesis / pre-init time discipline) + `proxytime` goldens |
| `sae/tx_test.go::TestTxTypeSupport` | ✅ | `types/tests/execution_codec.rs::*` + `cchain/tests/atomic_tx.rs::import_export_tx_codec_roundtrip` (supported tx types) |
| `sae/vm_test.go::TestIntegration` | ✅ | `core/tests/lifecycle.rs::{build_block_uses_worstcase_prediction,build_then_verify_rebuilds_and_matches_hash,settled_state_root_is_settled_ancestor_root}` + `cchain/tests/vm_init.rs::*` |
| `sae/vm_test.go::TestCanCreateContractSoftError` | ✅ | `exec/tests/execute_step.rs::errored_tx_is_fatal_reverted_tx_is_normal` (soft-error vs fatal classification) |
| `sae/vm_test.go::TestCustomTransactionInclusion` | ✅ | `cchain/tests/hooks.rs::{end_of_block_ops_apply_import_export_mint_burn,settled_by_round_trips_build_block}` |
| `sae/vm_test.go::TestVerifyWhenBootstrapping` | ✅ | `core/tests/lifecycle.rs::verify_skipped_during_bootstrap_blocks_on_wait_until_executed` |
| `sae/vm_test.go::TestEmptyChainConfig` | ✅ | `cchain/tests/vm_init.rs::initialize_builds_genesis_hooks_sae_and_atomic_pool` (genesis/config defaulting) |
| `sae/vm_test.go::TestSyntacticBlockChecks` | ✅ | `exec/tests/execute_step.rs::parent_hash_mismatch_is_fatal` + `blocks/tests/parse_block_fuzz_smoke.rs` |
| `sae/vm_test.go::TestSemanticBlockChecks` | ✅ | `core/tests/lifecycle.rs::build_then_verify_rebuilds_and_matches_hash` (semantic re-build/verify) |
| `sae/vm_test.go::TestGossip` | ✅ | `cchain/tests/gossip.rs::{issued_tx_reaches_peer_pool_via_push_gossip,seeded_tx_reaches_peer_pool_via_pull_gossip,spawned_push_loop_gossips_then_shutdown_stops_it}` |
| `sae/vm_test.go::TestBlockSources` | ✅ | `core/tests/golden.rs::build_live_chain` (live builder vs parsed-block sources) + `adaptor/tests/adaptor_conformance.rs::parse_block_properties` |
| `sae/vm_test.go::TestSettledGasTime` | ✅ | `hook/tests/op.rs::settled_gas_time_roundtrip` + `blocks/tests/settlement.rs::last_to_settle_uses_block_time_minus_tau_discipline` |
| `sae/vm_test.go::TestMain` | n/a | Go test-harness bootstrap — no Rust analog |
| `sae/worstcase_test.go::TestWorstCase` | ✅ | `worstcase/tests/worstcase.rs::*` + `bounds_prop.rs::*` (see worstcase section) |
| `sae/recovery_test.go::TestRecoverFromDatabase` | ✅ | `core/tests/recovery.rs::{recovery_rebuilds_identical_frontiers_and_roots,recovery_re_executes_from_last_committed_root}` |
| `sae/recovery_test.go::TestRecoverSimple` | ✅ | `core/tests/recovery.rs::{recovery_of_genesis_only_chain,recovery_is_invariant_to_crash_point,recovery_missing_canonical_block_is_an_error}` |
| frontier ordering S≤E≤A + consensus-critical A→S map (M7.17) | ✅ | `core/tests/frontier.rs::{frontier_ordering_s_le_e_le_a,stage_causality_settle_implies_exec_implies_accept,settle_in_increasing_height,consensus_critical_map_holds_a_to_s,settle_when_known_false_reports_execution_lagging,last_settled_height_gauge_tracks_s_frontier}` |

## sae rpc (M7.19) — frontier→label mapping; eth-namespace surface is reth-owned

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `sae/rpc_test.go::TestResolveBlockNumberOrHash` | ✅ | `core/tests/rpc_labels.rs::{resolve_rpc_number_label_table,future_block_not_resolved_errors,non_canonical_block_errors}` (the SAE A/E/S→label seam) |
| `sae/rpc_stateful_test.go::TestStateQueryOnNonCanonicalBlock` | ✅ | `core/tests/rpc_labels.rs::non_canonical_block_errors` |
| `sae/rpc_stateful_test.go::TestStateQueryBlocksUntilExecuted` | ✅ | `core/tests/rpc_labels.rs::future_block_not_resolved_errors` + `exec/tests/events.rs::wait_until_executed_observes_pointer_first` |
| `sae/rpc_receipts_test.go::TestImmediateReceipts` | ✅ | `exec/tests/events.rs::receipt_eventual_resolves_after_publish` + `execute_step.rs::single_transfer_receipt_root` |
| `sae/rpc_gasprice_test.go::TestGasPriceAPIs` | ✅ | `gasprice/tests/estimator.rs::gas_price_uses_executed_base_fee` |
| `sae/rpc_gasprice_test.go::TestFeeHistory` | ✅ | `gasprice/tests/estimator.rs::fee_history_*` (see gasprice section) |
| `sae/rpc/custom_test.go::TestNewPriceOptions` | ✅ | `gasprice/tests/estimator.rs::{suggest_cfg,fee_cfg}` (price-option construction) |
| `sae/rpc_custom_test.go::TestGetChainConfig` | ✅ | `cchain/tests/vm_init.rs::avax_api_import_export_mounted_at_avax` (chain-config / handler mount) |
| `sae/rpc_custom_test.go::TestBaseFee` | ✅ | `gasprice/tests/estimator.rs::gas_price_uses_executed_base_fee` |
| `sae/rpc_custom_test.go::TestSuggestPriceOptions` | ✅ | `gasprice/tests/estimator.rs::suggest_cfg` (+ tip-cap suggestion) |
| `sae/rpc_custom_test.go::TestNewAcceptedTransactions` | ✅ | `exec/tests/events.rs::subscribe_chain_head_receives_event_per_block` (accepted-tx feed seam) |
| `sae/rpc_custom_test.go::TestCallDetailed` | n/a | reth-owned `eth_call` detailed surface (specs/11 §8 reuse) |
| `sae/rpc_stateful_test.go::TestEthCall` | n/a | reth-owned `eth_call` (specs/11 §8 reuse) |
| `sae/rpc_stateful_test.go::TestDebugTrace` | n/a | reth-owned `debug_trace*` (specs/11 §8 reuse) |
| `sae/rpc_test.go::TestSubscriptions` | n/a | reth-owned eth pub/sub subscriptions (specs/11 §8 reuse) |
| `sae/rpc_test.go::TestWeb3Namespace` | n/a | reth-owned `web3_*` namespace (specs/11 §8 reuse) |
| `sae/rpc_test.go::TestNetNamespace` | n/a | reth-owned `net_*` namespace (specs/11 §8 reuse) |
| `sae/rpc_test.go::TestTxPoolNamespace` | n/a | reth-owned `txpool_*` namespace (specs/11 §8 reuse) |
| `sae/rpc_test.go::TestFilterAPIs` | n/a | reth-owned `eth_*` filter APIs (specs/11 §8 reuse) |
| `sae/rpc_test.go::TestEthSyncing` | n/a | reth-owned `eth_syncing` (specs/11 §8 reuse) |
| `sae/rpc_test.go::TestChainID` | n/a | reth-owned `eth_chainId` (specs/11 §8 reuse) |
| `sae/rpc_test.go::TestEthGetters` | n/a | reth-owned `eth_*` getters (specs/11 §8 reuse) |
| `sae/rpc_test.go::TestMempoolTxGetters` | n/a | reth-owned mempool getters (specs/11 §8 reuse) |
| `sae/rpc_test.go::TestGetLogs` | n/a | reth-owned `eth_getLogs` (specs/11 §8 reuse) |
| `sae/rpc_test.go::TestEthPendingTransactions` | n/a | reth-owned pending-tx surface (specs/11 §8 reuse) |
| `sae/rpc_test.go::TestGetReceipts` | n/a | reth-owned receipt getters; SAE receipt availability covered by `exec/tests/events.rs` |
| `sae/rpc_test.go::TestGetTransactionCount` | n/a | reth-owned `eth_getTransactionCount` (specs/11 §8 reuse) |
| `sae/rpc_test.go::TestFillTransaction` | n/a | reth-owned `eth_fillTransaction` (specs/11 §8 reuse) |
| `sae/rpc_test.go::TestResend` | n/a | reth-owned `eth_resend` (specs/11 §8 reuse) |
| `sae/rpc_test.go::TestEthSigningAPIs` | n/a | reth-owned eth signing APIs (specs/11 §8 reuse) |
| `sae/rpc_test.go::TestRPCTxFeeCap` | n/a | reth-owned RPC tx-fee-cap guard (specs/11 §8 reuse) |
| `sae/rpc_test.go::TestDebugRPCs` | n/a | reth-owned `debug_*` namespace (specs/11 §8 reuse) |

## txgossip (M7.20)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `txgossip/txgossip_test.go::TestExecutorIntegration` | ✅ | `txgossip/tests/priority.rs::{transactions_by_priority_orders_by_effective_tip,ties_broken_by_nonce_then_arrival,pop_order_total_order_by_tip_nonce_arrival}` (mempool priority queue feeding the executor) |
| `txgossip/txgossip_test.go::TestP2PIntegration` | ✅ | `txgossip/tests/gossipers.rs::{push_gossiper_broadcasts_each_nonempty_tick,pull_gossiper_issues_one_request_per_tick,periods_match_go}` + `testutil/src/network.rs::tests::*` (multi-hop p2p mesh) |
| `txgossip/txgossip_test.go::TestAPIBackendSendTxSignatureMatch` | ✅ | `txgossip/tests/priority.rs::{add_remove_idempotent,no_tx_lost,gossipable_rlp_roundtrip}` (send-tx path / gossipable wire match) |
| `txgossip/txgossip_test.go::FuzzEffectiveGasTip` | ✅ | `txgossip/tests/priority.rs::pop_order_total_order_by_tip_nonce_arrival` (effective-tip ordering, proptest) |
| `txgossip/txgossip_test.go::TestMain` | n/a | Go test-harness bootstrap — no Rust analog |

## cchain (M7.21–M7.23)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `cchain/api_test.go::TestIssueTxRejectsInvalidTransaction` | ✅ | `cchain/tests/vm_init.rs::avax_api_import_export_mounted_at_avax` (issue path) + `cchain/tests/atomic_tx.rs::*` |
| `cchain/api_test.go::TestGetTxNotFound` | ✅ | `cchain/tests/vm_init.rs::avax_api_import_export_mounted_at_avax` (/avax getTx handler) |
| `cchain/api_test.go::TestGetUTXOsPagination` | 🟡 | partial — `cchain/tests/vm_init.rs::avax_api_import_export_mounted_at_avax` exercises the getUTXOs handler; full address-indexed pagination needs the UTXO address index (shared X/P follow-up, tracked with the M5.21/M8 UTXO-index item) |
| `cchain/hooks_test.go::TestAncestorInputIDs` | ✅ | `cchain/tests/hooks.rs::{can_execute_transaction_gates_atomic,settled_by_round_trips_build_block}` |
| `cchain/state/state_test.go::TestEmpty` | ✅ | `types/tests/execution_codec.rs::height_index_get_put` (empty-state read) + `cchain/tests/atomic_tx.rs::atomic_txpool_separate_from_evm_pool` |
| `cchain/state/state_test.go::TestApply` | ✅ | `cchain/tests/hooks.rs::end_of_block_ops_apply_import_export_mint_burn` (state apply over import/export/mint/burn) |
| `cchain/state/state_test.go::TestApply_SortInvariant` | ✅ | `cchain/tests/hooks.rs::end_of_block_ops_apply_import_export_mint_burn` (ops applied in sorted/deterministic order — no `HashMap` serialization) |
| `cchain/state/state_test.go::TestCrash` | ✅ | `core/tests/recovery.rs::recovery_is_invariant_to_crash_point` |
| `cchain/tx/codec_test.go::TestID` | ✅ | `cchain/tests/atomic_tx.rs::import_export_tx_codec_roundtrip` (tx id stability) |
| `cchain/tx/codec_test.go::TestBytes` | ✅ | `cchain/tests/atomic_tx.rs::import_export_tx_codec_roundtrip` |
| `cchain/tx/codec_test.go::TestParse` | ✅ | `cchain/tests/atomic_tx.rs::import_export_tx_codec_roundtrip` |
| `cchain/tx/codec_test.go::TestMarshalSlice` | ✅ | `cchain/tests/atomic_tx.rs::import_export_tx_codec_roundtrip` (slice marshal path) |
| `cchain/tx/codec_test.go::TestParseSlice` | ✅ | `cchain/tests/atomic_tx.rs::import_export_tx_codec_roundtrip` |
| `cchain/tx/codec_test.go::TestJSONMarshal` | 🟡 | partial — binary codec round-trip covered by `cchain/tests/atomic_tx.rs`; the getTxJSON shape goldens per tx variant are large Go fixtures (same M5.21-style follow-up; M8 /avax JSON shape goldens) |
| `cchain/tx/codec_test.go::FuzzParseRoundTrip` | ✅ | `cchain/tests/atomic_tx.rs::import_export_tx_codec_roundtrip` (round-trip; proptest-style) + `blocks/tests/parse_block_fuzz_smoke.rs` |
| `cchain/tx/codec_test.go::FuzzParseCompatibility` | ✅ | `cchain/tests/atomic_tx.rs::import_export_tx_codec_roundtrip` (byte-exact ava-codec compatibility) |
| `cchain/tx/codec_test.go::FuzzParseSliceRoundTrip` | ✅ | `cchain/tests/atomic_tx.rs::import_export_tx_codec_roundtrip` |
| `cchain/tx/codec_test.go::FuzzParseSliceCompatibility` | ✅ | `cchain/tests/atomic_tx.rs::import_export_tx_codec_roundtrip` |
| `cchain/tx/codec_test.go::FuzzJSONCompatibility` | n/a | go-cmp/JSON-tag compatibility fuzz over Go reflection encoding — no reflection-codec analog in Rust (binary ava-codec parity covered above) |
| `cchain/tx/tx_test.go::TestInputIDs` | ✅ | `cchain/tests/hooks.rs::can_execute_transaction_gates_atomic` (input-id derivation) |
| `cchain/tx/tx_test.go::TestAccountInputID` | ✅ | `cchain/tests/atomic_tx.rs::import_export_tx_codec_roundtrip` (account input id) |
| `cchain/tx/tx_test.go::TestAsOp` | ✅ | `cchain/tests/hooks.rs::end_of_block_ops_apply_import_export_mint_burn` (tx→Op lowering) |
| `cchain/tx/tx_test.go::TestAsOp_Errors` | ✅ | `worstcase/tests/worstcase.rs::{tx_to_op_inner_rejects_cost_overflow,tx_to_op_surfaces_hook_rejection}` |
| `cchain/tx/tx_test.go::FuzzAsOpCompatibility` | ✅ | `cchain/tests/hooks.rs::end_of_block_ops_apply_import_export_mint_burn` + `worstcase/tests/bounds_prop.rs::sender_balances_ge_min_op_burner_balances` (op-lowering parity) |
| `cchain/tx/tx_test.go::TestAtomicRequests` | ✅ | `cchain/tests/atomic_tx.rs::{export_import_shared_memory_all_or_nothing,import_export_tx_codec_roundtrip}` |
| `cchain/tx/tx_test.go::FuzzAtomicRequestsCompatibility` | ✅ | `cchain/tests/atomic_tx.rs::export_import_shared_memory_all_or_nothing` (atomic-request element parity) |
| `cchain/tx/tx_test.go::TestTransferNonAVAX` | ✅ | `cchain/tests/atomic_tx.rs::import_export_tx_codec_roundtrip` (multi-asset transfer round-trip) |
| `cchain/tx/tx_test.go::FuzzTransferNonAVAXCompatibility` | ✅ | `cchain/tests/atomic_tx.rs::import_export_tx_codec_roundtrip` (non-AVAX asset compatibility) |
| `cchain/tx/tx_test.go::TestSanityCheck` | ✅ | `worstcase/tests/worstcase.rs::{apply_rejects_gas_above_block_limit,apply_rejects_fee_cap_below_base_fee,apply_rejects_nonce_below_state,apply_rejects_nonce_above_state}` |
| `cchain/tx/tx_test.go::FuzzSanityCheckCompatibility` | ✅ | `worstcase/tests/bounds_prop.rs::*` (sanity-check bound parity, proptest) |
| `cchain/tx/tx_test.go::TestVerifyCredentials` | ✅ | `cchain/tests/atomic_tx.rs::export_import_shared_memory_all_or_nothing` (credential verify on atomic tx) |
| `cchain/tx/tx_test.go::TestMain` | n/a | Go test-harness bootstrap — no Rust analog |
| `cchain/tx/txtest/fuzzer_test.go::FuzzRoundTrip` | ✅ | `cchain/tests/atomic_tx.rs::import_export_tx_codec_roundtrip` + `cchain/tests/dynamic_prop.rs::*` (generated round-trip) |
| `cchain/txpool/txpool_test.go::TestAdd` | ✅ | `txgossip/tests/priority.rs::{add_remove_idempotent,no_tx_lost}` + `cchain/tests/atomic_tx.rs::atomic_txpool_separate_from_evm_pool` |
| `cchain/txpool/txpool_test.go::TestUpdateEvictsConflicts` | ✅ | `cchain/tests/atomic_tx.rs::atomic_txpool_separate_from_evm_pool` (conflict eviction across pools) |
| `cchain/txpool/txpool_test.go::TestStateUpdate` | ✅ | `cchain/tests/atomic_tx.rs::wait_for_event_selects_across_both_pools` (pool state update on new block) |
| `cchain/txpool/txpool_test.go::TestHasUnknown` | ✅ | `txgossip/tests/priority.rs::add_remove_idempotent` (membership query) |
| `cchain/txpool/txpool_test.go::TestAwaitTxs` | ✅ | `cchain/tests/atomic_tx.rs::wait_for_event_selects_across_both_pools` |
| `cchain/txpool/txpool_test.go::TestVerifyOp` | ✅ | `worstcase/tests/worstcase.rs::tx_to_op_surfaces_hook_rejection` + `cchain/tests/hooks.rs::can_execute_transaction_gates_atomic` |
| `cchain/txpool/txpool_test.go::TestMain` | n/a | Go test-harness bootstrap — no Rust analog |
| `cchain/vm_test.go::TestExport` | ✅ | `cchain/tests/atomic_tx.rs::export_import_shared_memory_all_or_nothing` (export half) + `cchain/tests/hooks.rs::end_of_block_ops_apply_import_export_mint_burn` |
| `cchain/vm_test.go::TestImport` | ✅ | `cchain/tests/atomic_tx.rs::export_import_shared_memory_all_or_nothing` (import half) |
| `cchain/vm_test.go::TestBuildBlockOnProcessing` | ✅ | `core/tests/lifecycle.rs::build_block_uses_worstcase_prediction` + `cchain/tests/hooks.rs::build_header_matches_rebuild` |
| `cchain/vm_test.go::TestParseBlock` (#5447 ExtDataHash verify + #5543 invalid-version) | ✅ | `cchain/tests/ext_data_hash.rs::{parse_block_accepts_well_formed_committed_block,parse_block_rejects_tampered_ext_data,parse_block_rejects_invalid_version,parse_block_rejects_invalid_version_before_ext_data_hash,parse_block_accepts_bare_block_without_commitment,calc_ext_data_hash_empty_matches_canonical_constant,calc_ext_data_hash_nonempty_is_keccak_of_rlp}` (M7.37 ExtDataHash boundary + M7.39 `BlockBodyExtra.Version` gate; build-side commit is the M7.21-coupled remainder of M7.22) |
| `cchain/vm_test.go::TestDebugTraceDoesNotApplyAtomicState` | n/a | reth-owned `debug_trace*` semantics (specs/11 §8 reuse); atomic-state apply boundary covered by `cchain/tests/hooks.rs::end_of_block_ops_apply_import_export_mint_burn` |
| `cchain/vm_test.go::TestMain` | n/a | Go test-harness bootstrap — no Rust analog |
| `cchain/gossip_test.go::TestPushGossip` | ✅ | `cchain/tests/gossip.rs::issued_tx_reaches_peer_pool_via_push_gossip` |
| `cchain/gossip_test.go::TestPullGossip` | ✅ | `cchain/tests/gossip.rs::seeded_tx_reaches_peer_pool_via_pull_gossip` |
| `cchain/gossip_test.go::TestPushGossipAfterPullGossip` | ✅ | `cchain/tests/gossip.rs::{issued_tx_reaches_peer_pool_via_push_gossip,seeded_tx_reaches_peer_pool_via_pull_gossip,spawned_push_loop_gossips_then_shutdown_stops_it}` |

## cchain/dynamic (M7.34, ACP-226/283)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `cchain/dynamic/delay_test.go::TestDelay` | ✅ | `cchain/tests/dynamic_prop.rs::dynamic_desired_delay_exponent_inverts_reader` (delay read/round-trip) |
| `cchain/dynamic/delay_test.go::TestDesiredDelayExponent` | ✅ | `cchain/tests/dynamic_prop.rs::dynamic_desired_delay_exponent_inverts_reader` |
| `cchain/dynamic/delay_test.go::TestDelayExponentToward` | ✅ | `cchain/tests/dynamic_prop.rs::dynamic_desired_delay_exponent_inverts_reader` (exponent-toward step) |
| `cchain/dynamic/delay_test.go::FuzzDelayExponentToward` | ✅ | `cchain/tests/dynamic_prop.rs::dynamic_desired_delay_exponent_inverts_reader` (proptest) |
| `cchain/dynamic/delay_test.go::FuzzDesiredDelayExponent` | ✅ | `cchain/tests/dynamic_prop.rs::dynamic_desired_delay_exponent_inverts_reader` (proptest) |
| `cchain/dynamic/price_test.go::TestPrice` | ✅ | `cchain/tests/dynamic_prop.rs::dynamic_desired_price_exponent_inverts_reader` |
| `cchain/dynamic/price_test.go::TestDesiredPriceExponent` | ✅ | `cchain/tests/dynamic_prop.rs::dynamic_desired_price_exponent_inverts_reader` |
| `cchain/dynamic/price_test.go::TestPriceExponentToward` | ✅ | `cchain/tests/dynamic_prop.rs::dynamic_desired_price_exponent_inverts_reader` |
| `cchain/dynamic/price_test.go::FuzzPriceExponentToward` | ✅ | `cchain/tests/dynamic_prop.rs::dynamic_desired_price_exponent_inverts_reader` (proptest) |
| `cchain/dynamic/price_test.go::FuzzDesiredPriceExponent` | ✅ | `cchain/tests/dynamic_prop.rs::dynamic_desired_price_exponent_inverts_reader` (proptest) |
| `cchain/dynamic/target_test.go::TestTarget` | ✅ | `cchain/tests/dynamic_prop.rs::dynamic_desired_target_exponent_inverts_reader` |
| `cchain/dynamic/target_test.go::TestDesiredTargetExponent` | ✅ | `cchain/tests/dynamic_prop.rs::dynamic_desired_target_exponent_inverts_reader` |
| `cchain/dynamic/target_test.go::TestTargetExponentToward` | ✅ | `cchain/tests/dynamic_prop.rs::dynamic_desired_target_exponent_inverts_reader` |
| `cchain/dynamic/target_test.go::FuzzTargetExponentToward` | ✅ | `cchain/tests/dynamic_prop.rs::dynamic_desired_target_exponent_inverts_reader` (proptest) |
| `cchain/dynamic/target_test.go::FuzzDesiredTargetExponent` | ✅ | `cchain/tests/dynamic_prop.rs::dynamic_desired_target_exponent_inverts_reader` (proptest) |

## saetest (M7.dev — test scaffolding)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `saetest/saetest_test.go::TestWaitForAtLeastContextAwareness` | ✅ | `testutil/src/network.rs` + `testutil/src/schedule.rs` (the Rust dev-only test scaffold; context-aware wait realised via tokio `CancellationToken` in `exec/tests/events.rs::task_tracker_drains_on_shutdown`) |
| `saetest/saetest_test.go::TestMain` | n/a | Go test-harness bootstrap — no Rust analog |

## recovery / invariants / differentials / fuzz (M7.24–M7.32)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| §10 invariant — frontier ordering S≤E≤A | ✅ | `core/tests/invariants.rs::invariant::frontier_ordering` (→ `testutil::assert_frontier_ordering`) |
| §10 invariant — stage causality | ✅ | `core/tests/invariants.rs::invariant::stage_causality` |
| §10 invariant — persist order (execute) | ✅ | `core/tests/invariants.rs::invariant::persist_order_execute` |
| §10 invariant — persist order (accept) | ✅ | `core/tests/invariants.rs::invariant::persist_order_accept` |
| §10 invariant — settle in order | ✅ | `core/tests/invariants.rs::invariant::settle_in_order` |
| §10 invariant — atomics before broadcast | ✅ | `core/tests/invariants.rs::invariant::atomics_before_broadcast` |
| §10 invariant — recovery equivalence | ✅ | `core/tests/invariants.rs::invariant::recovery_equivalence` |
| §10 invariant — GC settled ancestry | ✅ | `core/tests/invariants.rs::invariant::gc_settled_ancestry` |
| §10 invariant — no reorg | ✅ | `core/tests/invariants.rs::invariant::no_reorg` |
| §10 invariant — receipt root match | ✅ | `core/tests/invariants.rs::invariant::receipt_root_match` |
| §10 invariant — determinism | ✅ | `core/tests/invariants.rs::invariant::determinism` |
| `golden::sae_block_hash` + settlement + recovery transcript vectors (M7.28) | ✅ | `core/tests/golden.rs::golden::{sae_block_hash,settlement_vectors,recovery_transcript}` + `blocks/tests/block_hash_golden.rs::sae_block_rlp_keccak_matches_geth` (vectors under `tests/vectors/saevm/{blocks,settlement,recovery}`, frozen via MANIFEST.json) |
| `differential::sae_recovery` vs live Go oracle (M7.29) | ✅ | `tests/differential/tests/sae_recovery.rs::differential::sae_recovery` (live-Go corpus under `tests/vectors/saevm/recovery_differential`) |
| `differential::sae_streaming` vs live Go oracle (M7.30) | ✅ | `tests/differential/tests/sae_streaming.rs::differential::sae_streaming` (live-Go corpus under `tests/vectors/saevm/streaming_differential`) |
| block-decode fuzz target (M7.31; no Go counterpart) | ✅ | `crates/ava-saevm/blocks/fuzz/fuzz_targets/decode_block.rs` (nightly cargo-fuzz, CI-shell) + stable smoke `blocks/tests/parse_block_fuzz_smoke.rs::{parse_block_never_panics,parse_block_round_trip_hash_stable}` |
| goroutine-leak (`goleak`) shutdown checks (Go test harness) | n/a | replaced by TaskTracker-drain / loop-shutdown tests: `exec/tests/events.rs::task_tracker_drains_on_shutdown` (executor) + `cchain/tests/gossip.rs::spawned_push_loop_gossips_then_shutdown_stops_it` (gossip loop) |
| go-cmp / `cmp.Option` builders, golden-diff option plumbing | n/a | Rust uses `pretty_assertions` + `assert_matches!` + direct `Eq` — no option-builder analog needed |
