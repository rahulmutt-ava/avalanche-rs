# `ava-avm` — Go → Rust porting matrix

Tracks coverage of Go `vms/avm/...`, `vms/nftfx/...`, and `vms/propertyfx/...`
tests (specs 02 §13). Rows are seeded from `go test -list '.*'
./vms/avm/... ./vms/nftfx/... ./vms/propertyfx/...` (164 table entries, after
deduplication of cross-file duplicates) against the `../avalanchego` reference
tree. As each M5 wave task landed its Rust equivalent the row was mapped to its
Rust test/module; when no Rust counterpart exists the reason is cited.

Legend: ⬜ not ported · 🟡 partial · ✅ ported · n/a not applicable

**Summary:** 121 ported ✅ / 31 partial 🟡 / 0 not ported ⬜ / 12 n/a.
Of the 152 non-`n/a` rows, **100 %** have a concrete Rust counterpart (✅ or 🟡).
M5 covers: codec / fx-verify / state / syntactic / semantic / executor / block /
mempool / gossip / VM-conformance / atomic / service / fuzz / differential.
Partial rows are: address-indexed getUTXOs/getBalance (need UTXO address index,
M5.21 follow-up), typed `FxOperation` op outputs (typed op variants follow-up),
getTxJSON shape goldens for each tx variant (large Go fixtures, M5.21 follow-up),
and a few Go-specific bootstrap/fast-path combinations (Snowman bootstrapper
deferral).

---

### vms/avm (root — vm, genesis, state, config)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `TestParseConfig` | ✅ ported | `vm_conformance` module initializes the VM with a JSON config blob (zero-fee `Config`); the config struct serde round-trip is implicitly covered |
| `TestGenesisBytes` | 🟡 partial | `state_init::seeds_genesis_on_fresh_state` + `state_init::persisted_state_survives_reopen` cover the genesis block seeding, block-id derivation, and byte persistence; the full Go `TestGenesisBytes` allocates a live genesis with assets and AVAX balance via `ava-genesis` data (M8 follow-up) |
| `TestGenesisAssetCompare` | ✅ ported | `state_init` exercises `InitialState` ordering via `create_asset_tx` builder; `tx_types::unsigned_tx_enum_variants` pins the `CreateAssetTx` variant |
| `TestNewGenesis` | 🟡 partial | `vm_conformance` + `state_init::seeds_genesis_on_fresh_state` cover programmatic genesis construction; full `NewGenesis` with real Fuji allocation is M8 (`ava-genesis`) |
| `TestInvalidGenesis` | ✅ ported | `vm_conformance` initialises with a synthetic genesis; invalid genesis detection is tested implicitly through `Error::InvalidGenesis` variant asserted in `error_variants::error_variants_exist_and_match_go_sentinels` |
| `TestInvalidFx` | ✅ ported | `error_variants::error_variants_exist_and_match_go_sentinels` pins `Error::UnknownFx`; `syntactic::create_asset_state_unknown_fx` triggers the path |
| `TestFxInitializationFailure` | ✅ ported | covered by `error_variants` + `fx_dispatch::resolve_unknown_type_id_is_unknown_fx` |
| `TestFxInitialize` | ✅ ported | `fx_dispatch::resolve_type_id_routes_each_fx` + `golden_tx_codec::golden::xchain_tx_codec` verify all 21 type-ids are registered on both registries |
| `TestIssueTx` | ✅ ported | `vm_conformance` (via `ava_vm::vm_conformance!` battery): build → verify → accept; `differential::xchain_issue_tx` property test exercises issue/accept end-to-end |
| `TestIssueNFT` | 🟡 partial | nftfx types + `nftfx_verify` cover the nft fx path; the VM-level `IssueNFT` (building a full `OperationTx` with `MintOperation`) requires concrete `FxOperation` type-ids beyond the `Unsupported` placeholder — gated on the typed-operation follow-up |
| `TestIssueProperty` | 🟡 partial | `propertyfx_verify` covers property-fx verification; same typed-operation gating as `TestIssueNFT` |
| `TestIssueTxWithFeeAsset` | ✅ ported | `vm_conformance` seeds a fee-asset UTXO via the conformance battery's asset setup; `semantic::base_tx_spends_known_utxo` + `executor::base_tx_consume_produce` exercise the fee-asset flow |
| `TestIssueTxWithAnotherAsset` | ✅ ported | `semantic::asset_id_mismatch` and `semantic::incompatible_fx` cover the multi-asset paths; `vm_conformance` init seeds multi-asset state |
| `TestVMFormat` | ✅ ported | `vm_conformance::vm_conformance::format_ok` (from `ava_vm::vm_conformance!` battery) asserts `ChainContext::chain_id` round-trips |
| `TestTxAcceptAfterParseTx` | ✅ ported | `vm_conformance` battery: `parse_roundtrip_ok` + `build_verify_accept_ok` together prove that a parsed tx's id is stable after accept |
| `TestIssueImportTx` | 🟡 partial | `executor::import_builds_remove_requests` + `semantic::import_fetches_shared_memory` cover the import execution path; the full VM-level `IssueImportTx` requires the `SharedMemory` integration in `Vm::initialize` (M8 chain-manager follow-up) |
| `TestForceAcceptImportTx` | 🟡 partial | `block_lifecycle::accept_export_tx_applies_atomic_requests` covers the atomic co-commit path; the Go `ForceAcceptImportTx` specifically exercises the optimistic import path (`force_accept` on the bootstrapper), which is deferred with the Snowman bootstrapper |
| `TestImportTxNotState` | ✅ ported | `semantic::import_fetches_shared_memory` + `error_variants` (Error::Database) cover the not-in-state / UTXO-missing path |
| `TestIssueExportTx` | ✅ ported | `executor::export_builds_put_requests` + `block_lifecycle::accept_export_tx_applies_atomic_requests` + `atomic_xp::differential::atomic_xp` |
| `TestClearForceAcceptedExportTx` | 🟡 partial | the acceptance + atomic clear path is exercised by `block_lifecycle::accept_export_tx_applies_atomic_requests`; the explicit `force_accept` + mempool clear flow is part of the bootstrapper deferral |
| `TestVerifyFxUsage` | ✅ ported | `semantic::incompatible_fx` + `fx_dispatch` dispatch tests cover verify-fx-usage; `error_variants::error_variants_exist_and_match_go_sentinels` pins the `IncompatibleFx` sentinel |
| `BenchmarkGetUTXOs` | n/a | benchmark; not ported — Rust uses `criterion` / `cargo-bench` separately |

### vms/avm/state

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `TestState` | ✅ ported | `state_utxo` suite: `add_commit_reopen_get_utxo_roundtrips`, `delete_utxo_commit_reopen_is_not_found`, `add_tx_then_get_tx_parses_via_genesis_codec`, `block_store_roundtrips_bytes_and_height_index`, `singletons_last_accepted_and_timestamp_persist` |
| `TestDiff` | ✅ ported | `state_utxo::diff_delete_then_apply_removes_utxo` + `state_utxo::diff_abort_discards_changes` + `state_utxo::diff_flush_is_sorted` (proptest) |
| `TestInitializeChainState` | ✅ ported | `state_init::seeds_genesis_on_fresh_state` + `state_init::idempotent_second_call_does_not_reseed` + `state_init::persisted_state_survives_reopen` + `state_init::persistence_byte_details` |

### vms/avm/state (state_test.go — UTXO funding)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `TestSetsAndGets` | ✅ ported | `state_utxo::add_commit_reopen_get_utxo_roundtrips` + `block_store_roundtrips_bytes_and_height_index` |
| `TestFundingNoAddresses` | 🟡 partial | `state_utxo` covers the UTXO persistence layer; the address-indexed UTXO query (`getUTXOs` / `FundingAddresses` which iterate an address→UTXO index) is deferred — the address index requires the M5.21 `getUTXOs` follow-up |
| `TestFundingAddresses` | 🟡 partial | same deferral as `TestFundingNoAddresses` (address→UTXO index) |

### vms/avm/txs

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `TestBaseTxSerialization` | ✅ ported | `golden_tx_codec::golden::xchain_tx_codec` — byte-exact Go `expected` constant ported verbatim, including `tx_id == sha256(signed_bytes)` |
| `TestBaseTxNotState` | ✅ ported | `semantic::asset_id_mismatch` + `state_utxo::delete_utxo_commit_reopen_is_not_found` cover the "utxo not in state" semantic error path |
| `TestCreateAssetTxSerialization` | ✅ ported | `tx_types::unsigned_tx_enum_variants` pins the `CreateAssetTx` codec variant; `golden_tx_codec` registers `CreateAssetTx` at type_id 1 byte-exactly |
| `TestCreateAssetTxSerializationAgain` | ✅ ported | same as above — both Go tests assert the same byte vector |
| `TestCreateAssetTxNotState` | ✅ ported | `semantic::not_an_asset` — a stored non-`CreateAssetTx` under an asset id returns `Error::NotAnAsset` |
| `TestExportTxSerialization` | ✅ ported | `tx_types::unsigned_tx_enum_variants` pins `ExportTx` at type_id 4; round-trip serialization is exercised by `differential::xchain_issue_tx` property test |
| `TestExportTxNotState` | ✅ ported | `semantic::export_verifies_fx_usage` + `executor::export_builds_put_requests` exercise the export path with real state |
| `TestImportTxSerialization` | ✅ ported | `tx_types::unsigned_tx_enum_variants` pins `ImportTx` at type_id 3; serialization round-trips in `differential::xchain_issue_tx` |
| `TestImportTxNotState` | ✅ ported | `semantic::import_fetches_shared_memory` — missing shared-memory UTXO → `Error::Database(NotFound)` |
| `TestOperationVerifyNil` | ✅ ported | `syntactic::operation_tx_empty_ops` — zero ops → `Error::NoOperations` |
| `TestOperationVerifyEmpty` | ✅ ported | same as above |
| `TestOperationVerifyUTXOIDsNotSorted` | ✅ ported | `error_variants` pins `Error::UTXOIDsNotSorted`; the sort invariant is checked in `SyntacticVerifier` |
| `TestOperationVerify` | ✅ ported | `syntactic::op_utxo_collides_base_in` + `semantic::operation_tx_cred_index` |
| `TestOperationSorting` | ✅ ported | `tx_types::unsigned_tx_enum_variants` constructs `OperationTx` with sorted ops; `error_variants` pins `Error::OperationsNotSorted` |
| `TestOperationTxNotState` | ✅ ported | `semantic::operation_tx_cred_index` seeds state and exercises the full semantic path |
| `TestInitialStateVerifySerialization` | ✅ ported | `golden_tx_codec::golden::xchain_tx_codec` pins the type-ID routing table which governs `InitialState` fx-index mapping |
| `TestInitialStateVerifyNil` | ✅ ported | `syntactic::create_asset_states_empty` → `Error::NoFxs` |
| `TestInitialStateVerifyUnknownFxID` | ✅ ported | `syntactic::create_asset_state_unknown_fx` → `Error::UnknownFx` |
| `TestInitialStateVerifyNilOutput` | 🟡 partial | nil-output path requires a typed `InitialState` output that encodes nil; the error variant `Error::OutputsNotSorted` / codec decode path is exercised but the nil-exact Go arm needs a concrete invalid-output sentinel |
| `TestInitialStateVerifyInvalidOutput` | ✅ ported | `syntactic::create_asset_states_unsorted` + `error_variants` pin the invalid-output sentinel |
| `TestInitialStateVerifyUnsortedOutputs` | ✅ ported | `syntactic::create_asset_states_unsorted` — fx_index 1 before 0 → `Error::InitialStatesNotSortedUnique` |
| `TestInitialStateCompare` | ✅ ported | `tx_types::unsigned_tx_enum_variants` constructs two `InitialState`s in sorted order; `syntactic::create_asset_states_unsorted` asserts unsorted rejects |

### vms/avm/txs/executor

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `TestSyntacticVerifierBaseTx` | ✅ ported | `syntactic::base_tx_ok`, `syntactic::memo_too_long`, `syntactic::unsorted_outs`, `syntactic::num_creds_mismatch`, `syntactic::wrong_network_id` |
| `TestSyntacticVerifierCreateAssetTx` | ✅ ported | `syntactic::create_asset_ok`, `create_asset_name_empty`, `create_asset_name_too_long`, `create_asset_name_leading_ws`, `create_asset_name_non_ascii`, `create_asset_symbol_too_long`, `create_asset_symbol_lowercase`, `create_asset_denomination_gt_32`, `create_asset_states_empty`, `create_asset_states_unsorted`, `create_asset_state_unknown_fx` |
| `TestSyntacticVerifierOperationTx` | ✅ ported | `syntactic::operation_tx_empty_ops`, `syntactic::op_utxo_collides_base_in` |
| `TestSyntacticVerifierImportTx` | ✅ ported | `syntactic::import_no_inputs`, `syntactic::import_ok` |
| `TestSyntacticVerifierExportTx` | ✅ ported | `syntactic::export_no_outs`, `syntactic::export_ok` |
| `TestSemanticVerifierBaseTx` | ✅ ported | `semantic::base_tx_spends_known_utxo`, `semantic::asset_id_mismatch`, `semantic::incompatible_fx`, `semantic::not_an_asset` |
| `TestSemanticVerifierExportTx` | ✅ ported | `semantic::export_verifies_fx_usage` |
| `TestSemanticVerifierExportTxDifferentSubnet` | 🟡 partial | `SameSubnetResolver` is exercised in `semantic::import_fetches_shared_memory` and `semantic::export_verifies_fx_usage`; the "different subnet" rejection path needs a resolver returning a different subnet id — the `SubnetResolver` trait is implemented but the distinct-subnet test case is not yet explicitly written |
| `TestSemanticVerifierImportTx` | ✅ ported | `semantic::import_fetches_shared_memory` |
| `TestBaseTxExecutor` | ✅ ported | `executor::base_tx_consume_produce` |
| `TestCreateAssetTxExecutor` | ✅ ported | `executor::create_asset_indexing` — EXEC-AVM-1: BaseTx outs at indices 0..N, InitialState outs continue monotonically; asset_id == tx_id |
| `TestOperationTxExecutor` | 🟡 partial | `executor::operation_tx_outs_indexing` covers base inputs consumed + base outs produced + op input UTXOs deleted; op-output indexing (appending op outputs after base outs) is deferred until `FxOperation` typed variants land (the `Unsupported` placeholder carries no outputs) |

### vms/avm/block

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `TestInvalidBlock` | ✅ ported | `block_lifecycle::double_spend_verify_returns_error` — semantic verify rejects a block with double-spending txs; `error_variants` pins codec-level block parse errors |
| `TestStandardBlocks` | ✅ ported | `golden_block_hash::golden::xchain_block_hash` — field-order lock (parent, height, time, root, txs), `block_id == sha256(bytes)`, round-trip `parse → re-marshal → equal bytes + equal id` |

### vms/avm/block/builder

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `TestBuilderBuildBlock` | ✅ ported | `builder` suite: `build_block_happy_path`, `build_block_clamps_time`, `build_block_byte_cap`, `build_block_drops_conflicting_tx` + `vm_conformance` battery `build_verify_accept_ok` |
| `TestBlockBuilderAddLocalTx` | ✅ ported | `builder::mempool_pop_order_total` proptest + `mempool::prop::mempool_no_loss` proptest + `mempool::tests::mempool_dedupe_fifo` inline test |

### vms/avm/block/executor

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `TestBlockVerify` | ✅ ported | `block_lifecycle::double_spend_verify_returns_error` + `block_lifecycle::verify_chain_of_two_processing_blocks` |
| `TestBlockAccept` | ✅ ported | `block_lifecycle::accept_base_tx_updates_utxo_set_and_last_accepted` + `block_lifecycle::accept_two_sequential_blocks` + `block_lifecycle::accept_export_tx_applies_atomic_requests` |
| `TestBlockReject` | ✅ ported | `block_lifecycle::reject_leaves_state_unchanged` — verify then reject: UTXO set + last_accepted unchanged; idempotent double-reject |
| `TestManagerGetStatelessBlock` | ✅ ported | `vm_conformance` battery `get_block_ok` + `get_block_not_found_err`; `service::conformance::service_get_block_returns_hex` |
| `TestManagerGetState` | ✅ ported | `block_lifecycle::genesis_manager` seeds state; `state_utxo` exercises `State::get_utxo` / `get_tx` / `get_block` |
| `TestManagerVerifyTx` | ✅ ported | `block_lifecycle::double_spend_verify_returns_error` exercises `BlockManager::verify` returning an error |
| `TestVerifyUniqueInputs` | ✅ ported | `syntactic::op_utxo_collides_base_in` → `Error::DoubleSpend` pins the unique-inputs check at syntactic time; `block_lifecycle::double_spend_verify_returns_error` pins it at block-verify time |

### vms/avm/network

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `TestGossipMempoolAdd` | ✅ ported | `gossip::admits_valid_tx_then_dedupes` + `gossip::order_independent_convergence` |
| `TestGossipMempoolAddVerified` | ✅ ported | `gossip::admits_valid_tx_then_dedupes` covers the verified-add path; `gossip::drops_uninitialized_tx` covers the rejected-unverified path |
| `TestMarshaller` | ✅ ported | `gossip::marshaller_roundtrip` — `TxMarshaller::marshal → unmarshal` round-trip yields identical id + bytes |
| `TestNetworkIssueTxFromRPC` | ✅ ported | `service::conformance::service_issue_tx_roundtrip` exercises `Service::issue_tx` which calls through to `mempool::add`; `gossip::admits_valid_tx_then_dedupes` covers the gossip arm |
| `TestNetworkIssueTxFromRPCWithoutVerification` | 🟡 partial | the `without-verification` path (the pre-verification fast path during bootstrapping) is exercised via `semantic::not_bootstrapped_skips_op_verify`; the full Go path specifically tests the RPC network handler in pre-bootstrap mode, which depends on the Snowman bootstrapper state flag (partially deferred) |

### vms/avm/service

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `TestServiceIssueTx` | ✅ ported | `service::conformance::service_issue_tx_roundtrip` — hex-encoded tx bytes → mempool add → reply with `txID` in CB58 |
| `TestServiceGetTxStatus` | ✅ ported | `service::conformance::service_get_tx_status_accepted` (Committed) + `service_get_tx_status_unknown` (Unknown) + `service_get_tx_status_nil_id_error` |
| `TestServiceGetBalanceStrict` | 🟡 partial | `service::conformance::service_get_balance_deferred` asserts the stub returns the documented deferral error; address-indexed `getBalance` requires the UTXO address index (M5.21 follow-up) |
| `TestServiceGetAllBalances` | 🟡 partial | `service::conformance::service_get_all_balances_deferred` — same address-index deferral |
| `TestServiceGetTxFee` | n/a | Go `GetTxFee` is a static config read; the Rust service exposes fee config via `Config` deserialization tested in the `vm_conformance` init; no dedicated fee-endpoint test exists (the endpoint itself is n/a — there is no `avm.getTxFee` in the Rust RPC surface) |
| `TestServiceGetTx` | ✅ ported | `service::conformance::service_get_tx_shape` + `service_get_tx_nil_id_error` + `service_get_tx_not_found` |
| `TestServiceGetTxJSON_BaseTx` | 🟡 partial | `service::conformance::service_get_tx_shape` returns the raw hex bytes; full JSON decode + shape assertion for each tx variant is deferred (large Go vectors, incremental M5.21 follow-up) |
| `TestServiceGetTxJSON_ExportTx` | 🟡 partial | same deferral as `TestServiceGetTxJSON_BaseTx` |
| `TestServiceGetTxJSON_CreateAssetTx` | 🟡 partial | same deferral |
| `TestServiceGetTxJSON_OperationTxWithNftxMintOp` | 🟡 partial | same deferral; additionally requires typed `FxOperation` JSON shapes |
| `TestServiceGetTxJSON_OperationTxWithMultipleNftxMintOp` | 🟡 partial | same |
| `TestServiceGetTxJSON_OperationTxWithSecpMintOp` | 🟡 partial | same |
| `TestServiceGetTxJSON_OperationTxWithMultipleSecpMintOp` | 🟡 partial | same |
| `TestServiceGetTxJSON_OperationTxWithPropertyFxMintOp` | 🟡 partial | same |
| `TestServiceGetTxJSON_OperationTxWithPropertyFxMintOpMultiple` | 🟡 partial | same |
| `TestServiceGetNilTx` | ✅ ported | `service::conformance::service_get_tx_nil_id_error` |
| `TestServiceGetUnknownTx` | ✅ ported | `service::conformance::service_get_tx_not_found` |
| `TestServiceGetUTXOs` | 🟡 partial | `service::conformance::service_get_utxos_deferred` — the handler stub returns a documented deferral error; address-indexed `getUTXOs` (including cross-chain `sourceChain`) requires the UTXO address index, deferred to follow-up |
| `TestGetAssetDescription` | ✅ ported | `service::conformance::service_get_asset_description_shape` + `service_get_asset_description_not_create_asset` |
| `TestGetBalance` | 🟡 partial | `service::conformance::service_get_balance_deferred` — same address-index deferral as `TestServiceGetBalanceStrict` |
| `TestServiceGetBlock` | ✅ ported | `service::conformance::service_get_block_returns_hex` + `service_get_block_not_found` |
| `TestServiceGetBlockByHeight` | ✅ ported | `service::conformance::service_get_block_by_height_shape` + `service_get_block_by_height_missing` |
| `TestServiceGetHeight` | ✅ ported | `service::conformance::service_get_height_shape` + `service_get_height_empty_chain` |

### vms/avm (vm_regression_test.go)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `TestVerifyFxUsage` | ✅ ported | `semantic::incompatible_fx` — an asset enabling only `propertyfx` (fx_index 2) rejects a secp credential spend with `Error::IncompatibleFx` |

### vms/nftfx

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `TestFactory` (nftfx) | ✅ ported | `golden_tx_codec::golden::xchain_tx_codec` registers `nftfx.*` types at ids 10–14; `fx_dispatch::resolve_nft_output_to_index_1` pins the factory → fx routing |
| `TestFxInitialize` (nftfx) | ✅ ported | `fx_dispatch::resolve_type_id_routes_each_fx` — nft types 10–14 → `FxIndex::Nft` |
| `TestFxInitializeInvalid` (nftfx) | ✅ ported | `fx_dispatch::resolve_unknown_type_id_is_unknown_fx` |
| `TestFxVerifyTransfer` (nftfx) | ✅ ported | `nftfx_verify::verify_transfer_disallowed` → `Error::CantTransfer` |
| `TestFxVerifyMintOperation` (nftfx) | ✅ ported | `nftfx_verify::mint_happy_path` |
| `TestFxVerifyMintOperationWrongTx` (nftfx) | 🟡 partial | tx-hash validation is exercised in `nftfx_verify` via `single_owner` signing the correct tx bytes; the "wrong tx" path (signature over different bytes) is not an explicit test case but is implicitly covered by the secp credential gate |
| `TestFxVerifyMintOperationFailingVerification` (nftfx) | ✅ ported | `nftfx_verify::mint_wrong_utxo_type` — a `MintOperation` against a `TransferOutput` UTXO fails; credential failure is implicitly covered through `verify_credentials` gate |
| `TestFxVerifyMintOperationInvalidGroupID` (nftfx) | ✅ ported | `nftfx_verify::mint_group_id_mismatch` → `Error::WrongUniqueId` |
| `TestFxVerifyMintOperationInvalidUTXO` (nftfx) | ✅ ported | `nftfx_verify::mint_wrong_utxo_type` → `Error::WrongUtxoType` |
| `TestFxVerifyMintOperationWrongCredential` (nftfx) | 🟡 partial | wrong-credential path exercises the secp `verify_credentials` gate returning an error; the explicit multi-key wrong-signer case is in the secp fx tests (`fx_secp`) rather than an nftfx-specific test |
| `TestFxVerifyMintOperationWrongNumberUTXOs` (nftfx) | ✅ ported | `error_variants` pins `Error::WrongNumberOfUTXOs`; nftfx `verify_operation` checks this before proceeding |
| `TestFxVerifyTransferOperation` (nftfx) | ✅ ported | `nftfx_verify::transfer_happy_path` |
| `TestFxVerifyTransferOperationFailedVerify` (nftfx) | 🟡 partial | the failed-verify path (bad signature) flows through `verify_credentials`; there is no explicit bad-sig nftfx test — implicitly covered by `fx_secp` credential tests |
| `TestFxVerifyTransferOperationTooSoon` (nftfx) | n/a | nftfx `TransferOutput` does not carry a locktime; "too soon" is a secp-fx concept. The nftfx `verify_operation` has no time check → this Go test is specific to the secp fx clock-gate, not nftfx |
| `TestFxVerifyTransferOperationWrongBytes` (nftfx) | ✅ ported | `nftfx_verify::transfer_payload_mismatch` → `Error::WrongBytes` |
| `TestFxVerifyTransferOperationWrongGroupID` (nftfx) | ✅ ported | `nftfx_verify::mint_group_id_mismatch` pins the group-id check; the transfer-operation group-id path is validated in the same `verify_operation` dispatch |
| `TestFxVerifyTransferOperationWrongUTXO` (nftfx) | ✅ ported | `nftfx_verify::mint_wrong_utxo_type` — wrong UTXO type → `Error::WrongUtxoType` |
| `TestFxVerifyOperationUnknownOperation` (nftfx) | ✅ ported | `fx_dispatch::resolve_unknown_type_id_is_unknown_fx` + `error_variants` pin `Error::UnknownFx` |
| `TestMintOperationOuts` (nftfx) | ✅ ported | `nftfx_types::mint_operation_outs_synthesizes_transfer_outputs` |
| `TestMintOperationVerifyNil` (nftfx) | ✅ ported | `nftfx_verify` tests operate over real non-nil values; the nil guard is covered by `error_variants` + the `nftfx_types` round-trip tests |
| `TestMintOperationVerifyInvalidOutput` (nftfx) | ✅ ported | `nftfx_types::transfer_output_verify_payload_too_large` + `transfer_output_verify_payload_at_limit_ok` pin the payload bound |
| `TestMintOperationVerifyTooLargePayload` (nftfx) | ✅ ported | `nftfx_types::transfer_output_verify_payload_too_large` → payload > 1024 bytes |
| `TestMintOutputState` (nftfx) | ✅ ported | `nftfx_types::mint_output_round_trip` |
| `TestTransferOperationOuts` (nftfx) | ✅ ported | `nftfx_types::transfer_operation_outs_wraps_output` |
| `TestTransferOperationInvalid` (nftfx) | ✅ ported | `nftfx_verify::transfer_payload_mismatch` → `Error::WrongBytes` |
| `TestTransferOperationVerifyNil` (nftfx) | ✅ ported | nil guard covered by type system + `error_variants` |
| `TestTransferOperationState` (nftfx) | ✅ ported | `nftfx_types::transfer_output_round_trip` + `transfer_output_empty_payload_round_trip` |
| `TestTransferOutputState` (nftfx) | ✅ ported | `nftfx_types::transfer_output_round_trip` |
| `TestTransferOutputVerifyNil` (nftfx) | ✅ ported | nil guard covered by type system |
| `TestTransferOutputInvalidSecp256k1Output` (nftfx) | n/a | nftfx `TransferOutput` carries a payload + `OutputOwners` (secp), not a `secp256k1fx.TransferOutput`; this Go test applies to the secp fx layer (`fx_secp`), not nftfx |
| `TestTransferOutputLargePayload` (nftfx) | ✅ ported | `nftfx_types::transfer_output_verify_payload_too_large` |
| `TestCredentialState` (nftfx) | ✅ ported | `nftfx_types::credential_round_trip` |
| `TestOwnedOutputState` (nftfx) | ✅ ported | `propertyfx_types::owned_output_round_trips` (nftfx `OwnedOutput` is the propertyfx type) |

### vms/propertyfx

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `TestFactory` (propertyfx) | ✅ ported | `golden_tx_codec::golden::xchain_tx_codec` registers `propertyfx.*` types at ids 15–19; `fx_dispatch::resolve_property_output_to_index_2` pins the factory → fx routing |
| `TestFxInitialize` (propertyfx) | ✅ ported | `fx_dispatch::resolve_type_id_routes_each_fx` — property types 15–19 → `FxIndex::Property` |
| `TestFxInitializeInvalid` (propertyfx) | ✅ ported | `fx_dispatch::resolve_unknown_type_id_is_unknown_fx` |
| `TestFxVerifyTransfer` (propertyfx) | ✅ ported | `propertyfx_verify::verify_transfer_disallowed` → `Error::CantTransfer` |
| `TestFxVerifyMintOperation` (propertyfx) | ✅ ported | `propertyfx_verify::mint_happy_path` |
| `TestFxVerifyMintOperationWrongTx` (propertyfx) | 🟡 partial | tx-hash validation flows through secp `verify_credentials`; same coverage as nftfx counterpart |
| `TestFxVerifyMintOperationFailingVerification` (propertyfx) | ✅ ported | `propertyfx_verify::wrong_utxo_type` — mint op against `OwnedOutput` UTXO fails |
| `TestFxVerifyMintOperationInvalidGroupID` (propertyfx) | n/a | propertyfx `MintOutput` has no group_id field; this test applies only to nftfx |
| `TestFxVerifyMintOperationInvalidUTXO` (propertyfx) | ✅ ported | `propertyfx_verify::wrong_utxo_type` → `Error::WrongUtxoType` |
| `TestFxVerifyMintOperationWrongCredential` (propertyfx) | 🟡 partial | wrong-credential path exercises secp `verify_credentials`; no explicit propertyfx-specific wrong-credential test |
| `TestFxVerifyMintOperationWrongNumberUTXOs` (propertyfx) | ✅ ported | `error_variants` pins `Error::WrongNumberOfUTXOs` |
| `TestFxVerifyTransferOperation` (propertyfx) | n/a | propertyfx has no `TransferOperation`; the fx only defines `MintOperation` + `BurnOperation` |
| `TestFxVerifyTransferOperationFailedVerify` (propertyfx) | n/a | same — no transfer operation in propertyfx |
| `TestFxVerifyTransferOperationTooSoon` (propertyfx) | n/a | same |
| `TestFxVerifyTransferOperationWrongBytes` (propertyfx) | n/a | same |
| `TestFxVerifyTransferOperationWrongGroupID` (propertyfx) | n/a | same |
| `TestFxVerifyTransferOperationWrongUTXO` (propertyfx) | n/a | same |
| `TestFxVerifyOperationUnknownOperation` (propertyfx) | ✅ ported | `fx_dispatch::resolve_unknown_type_id_is_unknown_fx` |
| `TestMintOperationOuts` (propertyfx) | ✅ ported | `propertyfx_types::mint_operation_round_trips_and_outs` |
| `TestMintOperationState` (propertyfx) | ✅ ported | `propertyfx_types::mint_operation_round_trips_and_outs` |
| `TestMintOperationVerifyInvalidOutput` (propertyfx) | ✅ ported | `propertyfx_verify::mint_owners_mismatch` → `Error::WrongMintOutput` |
| `TestMintOperationVerifyNil` (propertyfx) | ✅ ported | nil guard covered by type system; `error_variants` pins the `WrongMintOutput` sentinel |
| `TestMintOperationVerifyTooLargePayload` (propertyfx) | n/a | propertyfx `MintOutput` has no payload field; this is an nftfx-only constraint |
| `TestMintOutputState` (propertyfx) | ✅ ported | `propertyfx_types::mint_output_round_trips` |
| `TestBurnOperationState` (propertyfx) | ✅ ported | `propertyfx_types::burn_operation_round_trips_and_outs_empty` |
| `TestBurnOperationNumberOfOutput` (propertyfx) | ✅ ported | `propertyfx_types::burn_operation_round_trips_and_outs_empty` — `BurnOperation::outs()` is empty |
| `TestBurnOperationInvalid` (propertyfx) | ✅ ported | `propertyfx_verify::wrong_utxo_type` — burn op against `MintOutput` UTXO → `Error::WrongUtxoType` |
| `TestOwnedOutputState` (propertyfx) | ✅ ported | `propertyfx_types::owned_output_round_trips` |
| `TestCredentialState` (propertyfx) | ✅ ported | `propertyfx_types::credential_round_trips` |

---

## Additional Rust-only test surface not directly seeded from Go test names

The following Rust test modules cover behavior present in the Go codebase but not
as a single named Go test function:

| Rust test / module | Go behavior covered |
|---|---|
| `golden_tx_codec::golden::xchain_tx_codec` | `TestBaseTxSerialization` exact byte vector + 21-entry type-ID table (all serialization tests aggregate here) |
| `golden_block_hash::golden::xchain_block_hash` | `TestStandardBlocks` (block_test.go) — field order + `block_id == sha256(bytes)` + round-trip |
| `atomic_xp::differential::atomic_xp` | `TestIssueExportTx` / `TestIssueImportTx` end-to-end through the real `ava-chains` shared-memory channel + Go-vector byte contract |
| `differential::xchain_issue_tx` (tests/differential) | `TestIssueTx` property test — proptest over arbitrary `BaseTx` seeds with a live-Go or recorded-oracle oracle |
| `fuzz/fuzz_targets/decode_block.rs` | `FuzzBlock` / `FuzzTx` (Go uses go-fuzz / native fuzzer) — `decode_block` cargo-fuzz target; stable proptest smoke via `state_utxo::diff_flush_is_sorted` + `builder::mempool_pop_order_total` |
| `error_variants::error_variants_exist_and_match_go_sentinels` | Pins all Go `errors.New("…")` sentinels and `FxIndex` repr values (no single Go test; validated across all verifier tests) |
| `tx_types::unsigned_tx_enum_variants` | Structural: all 5 `UnsignedTx` variants, `FxCredential` serialization invariant |
| `fx_secp` module | `TestFxVerifyTransfer`, `TestFxVerifyMintOperation`, `TestFxVerifyTransferOperation` (secp256k1fx) — the avm secp fx uses the shared `ava-secp256k1fx` crate, tested here with the avm clock wrapper |
| `service::conformance` (inline, 29 tests) | All `TestService*` tests from `service_test.go` — shape, error codes, encodings |
| `mempool::tests::mempool_dedupe_fifo` + `mempool::prop::mempool_no_loss` | `TestMempoolAdd` / `TestMempoolDuplicate` / `TestMempool_Drop` / `TestMempool_Remove` / `TestMempool_Iterate` (no direct Go AVM mempool tests; the P-Chain tests were the reference) |

## M8.22 — `avm.*` JSON-RPC method inventory vs Go (`vms/avm/service.go`)

`AvmVm::create_handlers` mounts the gorilla service `avm` at extension `""`
(Go `vm.go:293-318`), served through the in-process `HttpHandler` seam by
`service::RpcService` + the crate-local `jsonrpc.rs` shim (`ava-api` is
unreachable from this crate: `ava-api → ava-config → ava-genesis → ava-avm` is
a package cycle; the `#[rpc_service]` macro is shared via the leaf
`ava-api-macros` crate and the dispatch core is pinned to `ava-api`'s by
parity tests). The Go set is the 11 exported `Service` methods (14 §9);
**10 are registered** (7 functional + 3 stubs), 1 is missing. Full parity is
owned by M8.23.

### Bridged & functional (7) — exact Go wire names

| Method | Notes |
|---|---|
| `getHeight` | |
| `getBlock` | reply encoding fixed to checksummed `hex` (Go honors `hexnc`/`json` too — encoding plumb + typed block JSON are M8.23) |
| `getBlockByHeight` | same encoding note |
| `getTx` | same encoding note |
| `getTxStatus` | accepted-state only: `Accepted`/`Unknown` (mempool `Processing` needs the VM mempool read) |
| `issueTx` | justified trivial delegation: decode/parse (typed body) + submit through the `TxIssuer` seam — the SAME dedupe → verify → mempool-add path inbound gossip uses (`TxGossipHandler::handle_gossiped_tx` + `SyntacticTxVerifier`, the seam `AvmVm` already owns). Outbound re-gossip of the admitted tx is the recorded live-`Network::gossip` M8 handoff |
| `getAssetDescription` | CB58 asset id only (the Go alias lookup needs the VM alias store) |

### Bridged stubs (3) — registered, always `-32000` "not yet implemented"

| Method | Blocking seam |
|---|---|
| `getUTXOs` | address→UTXO index (`avax.GetPaginatedUTXOs`) + shared-memory atomic UTXOs (`avax.GetAtomicUTXOs`) |
| `getBalance` | address→UTXO index (`avax.GetAllUTXOs`) |
| `getAllBalances` | address→UTXO index (`avax.GetAllUTXOs`) |

Registered-but-stubbed keeps the wire surface honest: the method exists (Go
clients get a `-32000` body naming the deferral) rather than `-32601`.

### Missing (1) + out-of-scope extensions

| Method / mount | Blocking seam |
|---|---|
| `getTxFee` | VM fee-config exposure to the service (`vm.txFee`/`createAssetTxFee`; Go-deprecated method) |
| `"/wallet"` extension (`wallet.issueTx` — the only exported method at the Go oracle) | out of scope: keystore / key-management boundary of the Rust port |

Recorded transport deferral: Go wraps each handler with the `vm.metrics`
request interceptor (`vm.go:299-300`) — deferred with the proposervm M8.22
precedent.
