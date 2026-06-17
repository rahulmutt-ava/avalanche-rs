# `ava-platformvm` — Go → Rust porting matrix

Tracks coverage of Go `vms/platformvm/...` tests (specs 02 §13). Rows are seeded
from `go test -list '.*' ./vms/platformvm/...` (440 entries) against the
`../avalanchego` reference tree. As each M4 wave task lands its Rust equivalent
(`golden::*` / `prop::*` / `conformance::*` / `differential::*`), flip the row to
✅ and note the Rust test name.

Legend: ⬜ not ported · 🟡 partial · ✅ ported

| Go test | Status |
|---|---|
| `FuzzExpiryEntryLessAndMarshalOrdering` | ⬜ not ported |
| `FuzzExpiryEntryMarshal` | ⬜ not ported |
| `FuzzExpiryEntryUnmarshal` | ⬜ not ported |
| `FuzzGetFeeState` | ⬜ not ported |
| `FuzzGetValidatorFeeState` | ⬜ not ported |
| `FuzzMarshalDiffKeyByHeight` | ⬜ not ported |
| `FuzzMarshalDiffKeyBySubnetID` | ⬜ not ported |
| `FuzzStateCostOf` | ⬜ not ported |
| `FuzzStateSecondsRemaining` | ⬜ not ported |
| `FuzzSubnetIDNodeIDMarshal` | ⬜ not ported |
| `FuzzSubnetIDNodeIDOrdering` | ⬜ not ported |
| `FuzzSubnetIDNodeIDUnmarshal` | ⬜ not ported |
| `FuzzUnmarshalDiffKeyByHeight` | ⬜ not ported |
| `FuzzUnmarshalDiffKeyBySubnetID` | ⬜ not ported |
| `TestAbortBlock` | ⬜ not ported |
| `TestAcceptorVisitAbortBlock` | ⬜ not ported |
| `TestAcceptorVisitAtomicBlock` | ⬜ not ported |
| `TestAcceptorVisitCommitBlock` | ⬜ not ported |
| `TestAcceptorVisitProposalBlock` | ⬜ not ported |
| `TestAcceptorVisitStandardBlock` | ⬜ not ported |
| `TestAddAutoRenewedValidatorTxInitCtx` | ⬜ not ported |
| `TestAddAutoRenewedValidatorTxSerialization` | ⬜ not ported |
| `TestAddAutoRenewedValidatorTxSyntacticVerify` | ⬜ not ported |
| `TestAddDelegatorTxAddBeforeRemove` | ⬜ not ported |
| `TestAddDelegatorTxHeapCorruption` | ⬜ not ported |
| `TestAddDelegatorTxNotValidatorTx` | ⬜ not ported |
| `TestAddDelegatorTxOverDelegatedRegression` | ⬜ not ported |
| `TestAddDelegatorTxSyntacticVerify` | ⬜ not ported |
| `TestAddDelegatorTxSyntacticVerifyNotAVAX` | ⬜ not ported |
| `TestAddPermissionlessDelegatorTxNotValidatorTx` | ⬜ not ported |
| `TestAddPermissionlessDelegatorTxSyntacticVerify` | ⬜ not ported |
| `TestAddPermissionlessPrimaryDelegatorSerialization` | ⬜ not ported |
| `TestAddPermissionlessPrimaryValidator` | ✅ ported — byte-exact golden `golden_codec::pchain_tx_codec` (`AddPermissionlessValidatorTx`) + `prop_roundtrip::pchain_tx_roundtrip` |
| `TestAddPermissionlessSubnetDelegatorSerialization` | ⬜ not ported |
| `TestAddPermissionlessSubnetValidator` | ⬜ not ported |
| `TestAddPermissionlessValidatorTxNotDelegatorTx` | ⬜ not ported |
| `TestAddPermissionlessValidatorTxSyntacticVerify` | ⬜ not ported |
| `TestAddSubnetValidatorAccept` | ⬜ not ported |
| `TestAddSubnetValidatorMarshal` | ⬜ not ported |
| `TestAddSubnetValidatorReject` | ⬜ not ported |
| `TestAddSubnetValidatorTxNotDelegatorTx` | ⬜ not ported |
| `TestAddSubnetValidatorTxNotPermissionlessStaker` | ⬜ not ported |
| `TestAddSubnetValidatorTxNotValidatorTx` | ⬜ not ported |
| `TestAddSubnetValidatorTxSyntacticVerify` | ⬜ not ported |
| `TestAddThenDeleteValidatorMetadataWrite` | ⬜ not ported |
| `TestAddValidatorCommit` | ⬜ not ported |
| `TestAddValidatorDuringRemovalPostHelicon` | ⬜ not ported |
| `TestAddValidatorDuringRemovalPreHelicon` | ⬜ not ported |
| `TestAddValidatorInvalidNotReissued` | ⬜ not ported |
| `TestAddValidatorMetadataWrite` | ⬜ not ported |
| `TestAddValidatorProposalBlock` | ⬜ not ported |
| `TestAddValidatorReject` | ⬜ not ported |
| `TestAddValidatorTxNotDelegatorTx` | ⬜ not ported |
| `TestAddValidatorTxSyntacticVerify` | ⬜ not ported |
| `TestAddValidatorTxSyntacticVerifyNotAVAX` | ⬜ not ported |
| `TestAddressedCall` | ⬜ not ported |
| `TestAddressedCallBytes` | ⬜ not ported |
| `TestAdvanceTimeTo_PromotePendingDelegatorAndValidator` | ⬜ not ported |
| `TestAdvanceTimeTo_PromotePendingDelegatorAndValidator_PreservesRewardOrder` | ⬜ not ported |
| `TestAdvanceTimeTo_RemovesStaleExpiries` | ⬜ not ported |
| `TestAdvanceTimeTo_UpdateL1Validators` | ⬜ not ported |
| `TestAdvanceTimeTo_UpdatesFeeState` | ⬜ not ported |
| `TestAdvanceTimeTxAfterBanff` | ⬜ not ported |
| `TestAdvanceTimeTxDelegatorStakerWeight` | ⬜ not ported |
| `TestAdvanceTimeTxDelegatorStakers` | ⬜ not ported |
| `TestAdvanceTimeTxRemoveSubnetValidator` | ⬜ not ported |
| `TestAdvanceTimeTxTimestampTooEarly` | ⬜ not ported |
| `TestAdvanceTimeTxTimestampTooLate` | ⬜ not ported |
| `TestAdvanceTimeTxUnmarshal` | ⬜ not ported |
| `TestAdvanceTimeTxUpdatePrimaryNetworkStakers` | ⬜ not ported |
| `TestAdvanceTimeTxUpdateStakers` | ⬜ not ported |
| `TestAllocationCompare` | ⬜ not ported |
| `TestApricotProposalBlockTimeVerification` | ⬜ not ported |
| `TestApricotStandardBlockTimeVerification` | ⬜ not ported |
| `TestApricotStandardTxExecutorAddSubnetValidator` | ⬜ not ported |
| `TestAtomicBlock` | ⬜ not ported |
| `TestAtomicImport` | ⬜ not ported |
| `TestAtomicTxImports` | ⬜ not ported |
| `TestAuthComplexity` | ⬜ not ported |
| `TestBackendGetBlock` | ⬜ not ported |
| `TestBanffAbortBlockTimestampChecks` | ⬜ not ported |
| `TestBanffBlockSerialization` | ⬜ not ported |
| `TestBanffCommitBlockTimestampChecks` | ⬜ not ported |
| `TestBanffProposalBlockDelegatorStakerWeight` | ⬜ not ported |
| `TestBanffProposalBlockDelegatorStakers` | ⬜ not ported |
| `TestBanffProposalBlockJSON` | ⬜ not ported |
| `TestBanffProposalBlockRemoveSubnetValidator` | ⬜ not ported |
| `TestBanffProposalBlockTimeVerification` | ⬜ not ported |
| `TestBanffProposalBlockTrackedSubnet` | ⬜ not ported |
| `TestBanffProposalBlockUpdateStakers` | ⬜ not ported |
| `TestBanffStandardBlockDelegatorStakerWeight` | ⬜ not ported |
| `TestBanffStandardBlockRemoveSubnetValidator` | ⬜ not ported |
| `TestBanffStandardBlockTimeVerification` | ⬜ not ported |
| `TestBanffStandardBlockTrackedSubnet` | ⬜ not ported |
| `TestBanffStandardBlockUpdatePrimaryNetworkStakers` | ⬜ not ported |
| `TestBanffStandardBlockUpdateStakers` | ⬜ not ported |
| `TestBanffStandardBlockWithNoChangesRemainsInvalid` | ⬜ not ported |
| `TestBanffStandardTxExecutorAddValidator` | ⬜ not ported |
| `TestBaseStakersDelegator` | ⬜ not ported |
| `TestBaseStakersPruning` | ⬜ not ported |
| `TestBaseStakersValidator` | ⬜ not ported |
| `TestBaseTx` | ⬜ not ported |
| `TestBaseTxSerialization` | ⬜ not ported |
| `TestBlockExecutionWithComplexity` | ⬜ not ported |
| `TestBlockOptions` | ⬜ not ported |
| `TestBlockchainStatusJSON` | ⬜ not ported |
| `TestBlockchainStatusString` | ⬜ not ported |
| `TestBlockchainStatusVerify` | ⬜ not ported |
| `TestBootstrapPartiallyAccepted` | 🟡 `differential::pchain_sync_to_tip` (M4.29, in `src/vm.rs`) drives the M3 Snowman `Bootstrapper` over a **multi-block deterministic range** (5 empty Banff standard blocks, heights 1..=5, advancing chain time 105/205/…/505): frontier discovery → agreement → one `GetAncestors` answered with the full chain **tip-first/genesis-last** (`process_chain` walks parents within the reply; genesis is at the local last-accepted height → stops) → execute the range → handoff to NormalOp, asserting `last_accepted == tip_id` + `get_block(tip).height() == 5`. A second **per-height differential arm** on a fresh VM does `parse_block → verify → accept` and asserts `(block_id, timestamp, state_digest, getCurrentValidators sorted)` == the committed recorded-oracle corpus row at every height (`tests/vectors/platformvm/fuji_sync_oracle/linear_range.json`, generated behind `GENERATE_PCHAIN_SYNC_ORACLE=1`). `state_digest` is the P-Chain **flat-KV state-observation surrogate** (sha256 over `height ‖ last_accepted ‖ ts ‖ primary_supply ‖ sorted validators`), **NOT a merkle root** (`08` §3.2). The M4.27 height-0 subset is retained as `differential::pchain_sync_to_tip_height0`. ⬜ na — the byte-exact full-range arm vs the Go node is the **CI-gated `live-fuji` leg** (`differential::pchain_sync_to_tip_live_fuji`, a documented deferred stub run with `cargo nextest run -p ava-platformvm --features live-fuji` or `AVA_DIFF_LIVE=1`); it does NOT run in CI and does NOT affect the default build/test |
| `Node.initChains` / `chains.Manager.createSnowmanChain` (M4.30 binary boot) | ✅ The `avalanchers` binary boots the **real `PlatformVm`** in-process via `ava_chains::create_snowman_chain` → handler→engine-adapter path to `EngineState::Bootstrapping`, broadcasting `GetAcceptedFrontier` to its beacons (`avalanchers::wiring::chains::boot_in_process_pchain`; test `boots_real_pchain_to_bootstrapping`, verified mainnet genesis). Built in M4.30a (`ava-engine` `ChainEngine` adapters + transition mechanism), M4.30b (`create_snowman_chain` registers the adapters), M4.30c (`avalanchers` boots the real VM). ⬜ na — the real ava-network-backed `Sender` (engine→wire + timeout registration) and driving past Bootstrapping to NormalOp against live peers are the **gated live arm** (no network in CI); the in-process path uses a recording sender |
| `TestBoundedBy` | ⬜ not ported |
| `TestBuildBlockAdvanceTime` | ⬜ not ported |
| `TestBuildBlockBasic` | ⬜ not ported |
| `TestBuildBlockDoesNotBuildWithEmptyMempool` | ⬜ not ported |
| `TestBuildBlockForceAdvanceTime` | ⬜ not ported |
| `TestBuildBlockInvalidStakingDurations` | ⬜ not ported |
| `TestBuildBlockShouldReward` | ⬜ not ported |
| `TestByEndTime` | ⬜ not ported |
| `TestCommitBlock` | ⬜ not ported |
| `TestConfigUnmarshal` | ⬜ not ported |
| `TestConvertSubnetToL1TxSerialization` | ⬜ not ported |
| `TestConvertSubnetToL1TxSyntacticVerify` | ⬜ not ported |
| `TestConvertSubnetToL1ValidatorComplexity` | ⬜ not ported |
| `TestCreateChain` | ⬜ not ported |
| `TestCreateChainTxAP3FeeChange` | ⬜ not ported |
| `TestCreateChainTxInsufficientControlSigs` | ⬜ not ported |
| `TestCreateChainTxNoSuchSubnet` | ⬜ not ported |
| `TestCreateChainTxValid` | ⬜ not ported |
| `TestCreateChainTxWrongControlSig` | ⬜ not ported |
| `TestCreateSubnet` | ⬜ not ported |
| `TestCurrentStakers` | ⬜ not ported |
| `TestDeactivateLowBalanceL1ValidatorBlockChanges` | ⬜ not ported |
| `TestDeactivateLowBalanceL1Validators` | ⬜ not ported |
| `TestDelegatorAndValidatorExpireTogether` | ⬜ not ported |
| `TestDelegatorReplacementWeight` | ⬜ not ported |
| `TestDelegatorWeightAfterMultipleExpiration` | ⬜ not ported |
| `TestDeleteAddDeleteAddValidatorMetadataWrite` | ⬜ not ported |
| `TestDeleteL1Validator` | ⬜ not ported |
| `TestDeleteThenReAddValidatorMetadataWrite` | ⬜ not ported |
| `TestDeleteValidatorMetadataWrite` | ⬜ not ported |
| `TestDiffAccruedFees` | ⬜ not ported |
| `TestDiffChain` | ⬜ not ported |
| `TestDiffCurrentDelegator` | ⬜ not ported |
| `TestDiffCurrentSupply` | ⬜ not ported |
| `TestDiffCurrentValidator` | ⬜ not ported |
| `TestDiffExpiry` | ⬜ not ported |
| `TestDiffFeeState` | ⬜ not ported |
| `TestDiffIterationByHeight` | ⬜ not ported |
| `TestDiffIterationBySubnetID` | ⬜ not ported |
| `TestDiffL1ValidatorExcess` | ⬜ not ported |
| `TestDiffL1ValidatorsErrors` | ⬜ not ported |
| `TestDiffMissingState` | ⬜ not ported |
| `TestDiffMultipleBlocksRollback` | ⬜ not ported |
| `TestDiffMultipleValidatorsSameBlock` | ⬜ not ported |
| `TestDiffPendingDelegator` | ⬜ not ported |
| `TestDiffPendingValidator` | ⬜ not ported |
| `TestDiffRemoveValidatorNoPriorState` | ⬜ not ported |
| `TestDiffRewardUTXO` | ⬜ not ported |
| `TestDiffStacking` | ⬜ not ported |
| `TestDiffStakersAddDeleteAddDeleteValidator` | ⬜ not ported |
| `TestDiffStakersDelegator` | ⬜ not ported |
| `TestDiffStakersDeleteAddDeleteValidator` | ⬜ not ported |
| `TestDiffStakersDeleteThenReAddSameValidator` | ⬜ not ported |
| `TestDiffStakersDeleteValidator` | ⬜ not ported |
| `TestDiffStakersUpdateValidator` | ⬜ not ported |
| `TestDiffStakersValidator` | ⬜ not ported |
| `TestDiffStakingInfo` | ⬜ not ported |
| `TestDiffSubnet` | ⬜ not ported |
| `TestDiffSubnetOwner` | ⬜ not ported |
| `TestDiffSubnetToL1Conversion` | ⬜ not ported |
| `TestDiffTx` | ⬜ not ported |
| `TestDiffUTXO` | ⬜ not ported |
| `TestDiffValidatorReplacement` | ⬜ not ported |
| `TestDiffValidatorWeightDiffAfterDeleteAndAdd` | ⬜ not ported |
| `TestDisableL1ValidatorTxSerialization` | ✅ ported — byte-exact golden `golden_codec::pchain_tx_codec` (`DisableL1ValidatorTx`) + `prop_roundtrip::pchain_tx_roundtrip` |
| `TestDisableL1ValidatorTxSyntacticVerify` | ⬜ not ported |
| `TestDurangoDisabledTransactions` | ⬜ not ported |
| `TestDurangoMemoField` | ⬜ not ported |
| `TestDynamicCalculator` | ⬜ not ported |
| `TestEmpty` | ⬜ not ported |
| `TestEtnaCreateChainTxInvalidWithManagedSubnet` | ⬜ not ported |
| `TestEtnaDisabledTransactions` | ⬜ not ported |
| `TestEtnaStandardTxExecutorAddSubnetValidator` | ⬜ not ported |
| `TestFilterValidators` | ⬜ not ported |
| `TestGenesis` (platformvm/genesis: parse round-trip) | 🟡 `golden::pchain_genesis_block_id` covers parse/marshal round-trip + genesis-block derivation on a synthetic `Genesis` (M4.24) |
| `TestGenesis` (genesis: Fuji `expectedID = MSj6o9TpezwsQx4Tv7SHqpVvCbJ8of1ikjsqPZ1bKRjc9zBy3`) | ⬜ na — needs the full §3.1–§3.3 byte-exact construction pipeline (AVM + C-Chain genesis, bech32 allocation parsing, `txheap.ByEndTime` ordering) which lives in `ava-genesis` (M8); pin the exact-Fuji `p_chain_genesis_bytes`/`genesis_id` golden once M8 lands |
| `TestGenesisBytes` | ⬜ not ported |
| `TestGetBalance` | ⬜ not ported |
| `TestGetBlock` | ⬜ not ported |
| `TestGetBlock` | ⬜ not ported |
| `TestGetCanonicalValidatorSet` | ⬜ not ported |
| `TestGetCurrentValidators` | 🟡 `service::conformance::service_get_current_validators` (M4.28) — asserts the `getCurrentValidators` reply shape (field names `txID`/`nodeID`/`weight`/`startTime`/`publicKey`, avajson string-encoded ints, hex `0x…` BLS keys) and the canonical validation-id sorted order over the M4.21 `get_current_validator_set` seam. ⬜ na — exact-Go JSON golden deferred (no recorded `getCurrentValidators` vector; `tools/extract-vectors` has no P-Chain service surface yet, M4.24 precedent); the delegator/reward-owner/uptime fields of the full Go reply are deferred (out of scope for read-only sync — need the staker-attribute cache + owner formatting + delegator iteration) |
| `TestGetCurrentValidators` | 🟡 see row above (`service_get_current_validators`, M4.28) |
| `TestGetCurrentValidatorsForL1` | 🟡 `service_get_current_validators` includes L1 validators (the manager's `get_current_validator_set` merges base stakers + L1 validators, emitting `validationID`/`minNonce` for L1 entries); shape asserted, exact-Go JSON golden deferred (M4.28) |
| `TestGetDelegatorRules` | ⬜ not ported |
| `TestGetFeeConfig` | ⬜ not ported |
| `TestGetFeeStateErrors` | ⬜ not ported |
| `TestGetInputOutputs` | ⬜ not ported |
| `TestGetL1Validator` | 🟡 `service::Service::get_l1_validator` ported (M4.28): `getL1Validator` reply shape (`nodeID`/`weight`/`startTime`/`validationID`/`publicKey`/`minNonce`/`subnetID`/`height`) over the M4.20 `State::get_l1_validator` seam; balance/owner fields deferred (need codec-unmarshal of the stored owners + fee accounting), exact-Go JSON golden deferred |
| `TestGetNextStakerChangeTime` | ⬜ not ported |
| `TestGetNextStakerToReward` | ⬜ not ported |
| `TestGetProposedHeight` | ⬜ not ported |
| `TestGetPublicKeyDiffs` | ⬜ not ported |
| `TestGetStake` | ⬜ not ported |
| `TestGetStakerIteratorDeleteAndPut` | ⬜ not ported |
| `TestGetState` | ⬜ not ported |
| `TestGetTimestamp` | 🟡 `service::conformance::service_read_method_shapes` (M4.28) — asserts `getTimestamp` RFC3339 encoding (`time.Time` JSON) over `State::timestamp` |
| `TestGetTimestamp` | 🟡 see row above (`service_read_method_shapes`, M4.28) |
| `TestGetTx` | 🟡 `service::Service::get_tx_bytes` returns the raw stored tx bytes (M4.28); the encoding-selection / JSON-typed decode is deferred to the transport layer that owns `formatting.Encoding` |
| `TestGetTxStatus` | 🟡 `service::Service::get_tx_status` + `status.rs` `Status` enum ported (M4.28): accepted tx ⇒ `Committed`, absent ⇒ `Unknown`; the mempool / preferred-block `Processing` + dropped-reason paths are deferred (need the builder/mempool seam, read-only sync does not require them). `status::tests::status_json_roundtrip` pins the Go PascalCase JSON + discriminants |
| `TestGetValidatorFeeConfig` | ⬜ not ported |
| `TestGetValidatorRules` | ⬜ not ported |
| `TestGetValidatorSet_AfterEtna` | 🟡 `differential::validatorstate_parity` (M4.23) replays recorded P-Chain block sequences and asserts the M4.21 `PChainValidatorManager` backward diff-window reconstruction (`get_validator_set` at every height: weights + BLS keys, `NodeId`-ascending) matches a forward-accumulation oracle; also `conformance::validator_set_at_height` (M4.21). ⬜ na — byte-exact Go-extracted `validator_diff_windows` golden deferred: `tools/extract-vectors` has no P-Chain validator-diff-window surface yet; the committed vectors are a deterministic recorded oracle (forward-accumulation, an independent code path from the manager's backward reconstruction), per the M4.24 genesis precedent. Pin the exact Go golden once a tier-X extraction harness for `vms/platformvm/validators` lands |
| `TestGetValidatorsAt` | ⬜ not ported |
| `TestGetValidatorsAtArgsMarshalling` | ⬜ not ported |
| `TestGetValidatorsAtReplyMarshalling` | ⬜ not ported |
| `TestGetValidatorsSetProperty` | ⬜ not ported |
| `TestGetWarpValidatorSets` | 🟡 `differential::validatorstate_parity` (M4.23) asserts `get_warp_validator_sets` (flatten-by-key total weight + per-key entries) at every replayed height vs the forward oracle; also `conformance::warp_sets_flatten_and_dedup_by_key` (M4.21). ⬜ na — byte-exact Go-extracted golden deferred with `TestGetValidatorSet_AfterEtna` above |
| `TestGossipAddBloomFilter` | ⬜ not ported |
| `TestGossipMempoolAddVerificationError` | ⬜ not ported |
| `TestHash` | ⬜ not ported |
| `TestHashBytes` | ⬜ not ported |
| `TestHeightMarshalJSON` | ⬜ not ported |
| `TestHeightUnmarshalJSON` | ⬜ not ported |
| `TestIncreaseL1ValidatorBalanceTxSerialization` | ✅ ported — byte-exact golden `golden_codec::pchain_tx_codec` (`IncreaseL1ValidatorBalanceTx`) + `prop_roundtrip::pchain_tx_roundtrip` |
| `TestIncreaseL1ValidatorBalanceTxSyntacticVerify` | ⬜ not ported |
| `TestInputComplexity` | ⬜ not ported |
| `TestInterface` | ⬜ not ported |
| `TestInvalidAddValidatorCommit` | ⬜ not ported |
| `TestL1ValidatorAfterLegacyRemoval` | ⬜ not ported |
| `TestL1ValidatorRegistration` | ⬜ not ported |
| `TestL1ValidatorWeight` | ⬜ not ported |
| `TestL1ValidatorWeight_Verify` | ⬜ not ported |
| `TestL1Validator_Compare` | ⬜ not ported |
| `TestL1Validator_immutableFieldsAreUnmodified` | ⬜ not ported |
| `TestL1Validators` | ⬜ not ported |
| `TestLoadL1ValidatorAndLegacy` | ⬜ not ported |
| `TestLockInVerify` | ⬜ not ported |
| `TestLockOutVerify` | ⬜ not ported |
| `TestLongerDurationBonus` | ⬜ not ported |
| `TestManagerLastAccepted` | ⬜ not ported |
| `TestManagerSetPreference` | ⬜ not ported |
| `TestManagerSetPreferenceWithContext` | ⬜ not ported |
| `TestMarkAndIsInitialized` | ⬜ not ported |
| `TestMaxStakeAmount` | ⬜ not ported |
| `TestMempoolAdd` | ⬜ not ported |
| `TestMempoolDuplicate` | ⬜ not ported |
| `TestMempoolOrdering` | ⬜ not ported |
| `TestMempool_Drop` | ⬜ not ported |
| `TestMempool_Iterate` | ⬜ not ported |
| `TestMempool_Remove` | ⬜ not ported |
| `TestMempool_RemoveConflicts` | ⬜ not ported |
| `TestMempool_WaitForEvent` | ⬜ not ported |
| `TestMessage` | ⬜ not ported |
| `TestMutableStakerIterator` | ⬜ not ported |
| `TestNetworkIssueTxFromRPC` | ⬜ not ported |
| `TestNewApricotAbortBlock` | ⬜ not ported |
| `TestNewApricotAtomicBlock` | ⬜ not ported |
| `TestNewApricotCommitBlock` | ⬜ not ported |
| `TestNewApricotProposalBlock` | ⬜ not ported |
| `TestNewApricotStandardBlock` | ⬜ not ported |
| `TestNewBanffAbortBlock` | ⬜ not ported |
| `TestNewBanffCommitBlock` | ⬜ not ported |
| `TestNewBanffProposalBlock` | ⬜ not ported |
| `TestNewBanffStandardBlock` | ⬜ not ported |
| `TestNewCurrentStaker` | ⬜ not ported |
| `TestNewDiffOn` | ⬜ not ported |
| `TestNewExportTx` | ⬜ not ported |
| `TestNewImportTx` | ⬜ not ported |
| `TestNewInvalidEndtime` | ⬜ not ported |
| `TestNewInvalidStakeWeight` | ⬜ not ported |
| `TestNewInvalidUTXOBalance` | ⬜ not ported |
| `TestNewPendingStaker` | ⬜ not ported |
| `TestNewProofOfPossessionDeterministic` | ⬜ not ported |
| `TestNewReturnsSortedValidators` | ⬜ not ported |
| `TestNextBlockTime` | ⬜ not ported |
| `TestNoErrorOnUnexpectedSetPreferenceDuringBootstrapping` | ⬜ not ported |
| `TestNumSigners` | ⬜ not ported |
| `TestOptimisticAtomicImport` | ⬜ not ported |
| `TestOptionsUnexpectedBlockType` | ⬜ not ported |
| `TestOutputComplexity` | ⬜ not ported |
| `TestOwnerComplexity` | ⬜ not ported |
| `TestParse` | ⬜ not ported |
| `TestParse` | ⬜ not ported |
| `TestParseAddressedCallJunk` | ⬜ not ported |
| `TestParseDelegatorMetadata` | ⬜ not ported |
| `TestParseHashJunk` | ⬜ not ported |
| `TestParseJunk` | ⬜ not ported |
| `TestParseMessageJunk` | ⬜ not ported |
| `TestParseUnsignedMessageJunk` | ⬜ not ported |
| `TestParseValidatorMetadata` | ⬜ not ported |
| `TestParseWrongPayloadType` | ⬜ not ported |
| `TestParsedStateBlock` | ⬜ not ported |
| `TestPickFeeCalculator` | ⬜ not ported |
| `TestPreviouslyDroppedTxsCannotBeReAddedToMempool` | ⬜ not ported |
| `TestPrimaryNetworkValidatorPopulatedToEmptyBLSKeyDiff` | ⬜ not ported |
| `TestPriorityIsCurrent` | ⬜ not ported |
| `TestPriorityIsCurrentDelegator` | ⬜ not ported |
| `TestPriorityIsCurrentValidator` | ⬜ not ported |
| `TestPriorityIsDelegator` | ⬜ not ported |
| `TestPriorityIsPending` | ⬜ not ported |
| `TestPriorityIsPendingDelegator` | ⬜ not ported |
| `TestPriorityIsPendingValidator` | ⬜ not ported |
| `TestPriorityIsPermissionedValidator` | ⬜ not ported |
| `TestPriorityIsValidator` | ⬜ not ported |
| `TestProofOfPossession` | ⬜ not ported |
| `TestProposalBlocks` | ⬜ not ported |
| `TestProposalTxExecuteAddDelegator` | ⬜ not ported |
| `TestProposalTxExecuteAddSubnetValidator` | ⬜ not ported |
| `TestProposalTxExecuteAddValidator` | ⬜ not ported |
| `TestPruneMempool` | ⬜ not ported |
| `TestPutAndGetFeeState` | ⬜ not ported |
| `TestPutL1Validator` | ⬜ not ported |
| `TestRegisterL1Validator` | ⬜ not ported |
| `TestRegisterL1ValidatorTxSerialization` | ✅ ported — byte-exact golden `golden_codec::pchain_tx_codec` (`RegisterL1ValidatorTx`) + `prop_roundtrip::pchain_tx_roundtrip` |
| `TestRegisterL1ValidatorTxSyntacticVerify` | ⬜ not ported |
| `TestRegisterL1Validator_Verify` | ⬜ not ported |
| `TestReindexBlocks` | ⬜ not ported |
| `TestRejectBlock` | ⬜ not ported |
| `TestRejectedStateRegressionInvalidValidatorReward` | ⬜ not ported |
| `TestRejectedStateRegressionInvalidValidatorTimestamp` | ⬜ not ported |
| `TestRemovePermissionedValidatorDuringAddPending` | ⬜ not ported |
| `TestRemovePermissionedValidatorDuringPendingToCurrentTransitionNotTracked` | ⬜ not ported |
| `TestRemovePermissionedValidatorDuringPendingToCurrentTransitionTracked` | ⬜ not ported |
| `TestRemoveSubnetValidatorTxSerialization` | ⬜ not ported |
| `TestRemoveSubnetValidatorTxSyntacticVerify` | ⬜ not ported |
| `TestRestartFullyAccepted` | ⬜ not ported |
| `TestRewardAutoRenewedValidatorTxSerialization` | ⬜ not ported |
| `TestRewardAutoRenewedValidatorTxSyntacticVerify` | ⬜ not ported |
| `TestRewardDelegatorTxAndValidatorTxExecuteOnCommitPostDelegateeDeferral` | ⬜ not ported |
| `TestRewardDelegatorTxExecuteOnAbort` | ⬜ not ported |
| `TestRewardDelegatorTxExecuteOnCommitPostDelegateeDeferral` | ⬜ not ported |
| `TestRewardDelegatorTxExecuteOnCommitPreDelegateeDeferral` | ⬜ not ported |
| `TestRewardValidatorAccept` | ⬜ not ported |
| `TestRewardValidatorReject` | ⬜ not ported |
| `TestRewardValidatorTxExecuteOnAbort` | ⬜ not ported |
| `TestRewardValidatorTxExecuteOnCommit` | ⬜ not ported |
| `TestRewards` | ⬜ not ported |
| `TestRewardsMint` | ⬜ not ported |
| `TestRewardsOverflow` | ⬜ not ported |
| `TestServiceGetBlockByHeight` | 🟡 `service::Service::get_block_by_height` ported (M4.28): resolves the block id via `State::get_block_id_at_height` then returns the stored block bytes (`conformance::service_get_block_by_height_roundtrip` covers the missing-height error path); the encoding-selection / JSON block decode is deferred to the transport layer (`getBlock` likewise via `get_block`) |
| `TestServiceGetSubnets` | ⬜ not ported |
| `TestSetAutoRenewedValidatorConfigTxSerialization` | ⬜ not ported |
| `TestSetAutoRenewedValidatorConfigTxSyntacticVerify` | ⬜ not ported |
| `TestSetL1ValidatorWeightTxSerialization` | ✅ ported — byte-exact golden `golden_codec::pchain_tx_codec` (`SetL1ValidatorWeightTx`) + `prop_roundtrip::pchain_tx_roundtrip` |
| `TestSetL1ValidatorWeightTxSyntacticVerify` | ⬜ not ported |
| `TestSetUptimeAndSetStakingInfoBothPersist` | ⬜ not ported |
| `TestSignatureRequestVerify` | ⬜ not ported |
| `TestSignatureRequestVerifyL1ValidatorRegistrationNotRegistered` | ⬜ not ported |
| `TestSignatureRequestVerifyL1ValidatorRegistrationRegistered` | ⬜ not ported |
| `TestSignatureRequestVerifyL1ValidatorWeight` | ⬜ not ported |
| `TestSignatureRequestVerifySubnetToL1Conversion` | ⬜ not ported |
| `TestSignatureVerification` | ⬜ not ported |
| `TestSigner` | ⬜ not ported |
| `TestSignerComplexity` | ⬜ not ported |
| `TestSplit` | ⬜ not ported |
| `TestStakerDiffIterator` | ⬜ not ported |
| `TestStakerEquals` | ⬜ not ported |
| `TestStakerLess` | ⬜ not ported |
| `TestStandardBlocks` | ⬜ not ported |
| `TestStandardExecutorConvertSubnetToL1Tx` | ⬜ not ported |
| `TestStandardExecutorDisableL1ValidatorTx` | ⬜ not ported |
| `TestStandardExecutorIncreaseL1ValidatorBalanceTx` | ⬜ not ported |
| `TestStandardExecutorRegisterL1ValidatorTx` | ⬜ not ported |
| `TestStandardExecutorRemoveSubnetValidatorTx` | ⬜ not ported |
| `TestStandardExecutorSetL1ValidatorWeightTx` | ⬜ not ported |
| `TestStandardExecutorTransformSubnetTx` | ⬜ not ported |
| `TestStandardTxExecutorAddDelegator` | ⬜ not ported |
| `TestStandardTxExecutorAddValidatorTxEmptyID` | ⬜ not ported |
| `TestStateAccruedFeesCommitAndLoad` | ⬜ not ported |
| `TestStateAdvanceTime` | ⬜ not ported |
| `TestStateAndDiffIntegration_DeleteValidatorAndItsDelegator` | ⬜ not ported |
| `TestStateAndDiffIntegration_StakingInfo` | ⬜ not ported |
| `TestStateCostOf` | ⬜ not ported |
| `TestStateCostOfOverflow` | ⬜ not ported |
| `TestStateExpiryCommitAndLoad` | ⬜ not ported |
| `TestStateFeeStateCommitAndLoad` | ⬜ not ported |
| `TestStateL1ValidatorExcessCommitAndLoad` | ⬜ not ported |
| `TestStateSecondsRemaining` | ⬜ not ported |
| `TestStateSecondsRemainingLimit` | ⬜ not ported |
| `TestStateSubnetOwner` | ⬜ not ported |
| `TestStateSubnetToL1Conversion` | ⬜ not ported |
| `TestStateSyncGenesis` | ⬜ not ported |
| `TestState_ApplyValidatorDiffs` | ⬜ not ported |
| `TestState_writeStakers` | ⬜ not ported |
| `TestStatusJSON` | ⬜ not ported |
| `TestStatusString` | ⬜ not ported |
| `TestStatusVerify` | ⬜ not ported |
| `TestSubnetToL1Conversion` | ⬜ not ported |
| `TestSubnetToL1ConversionID` | ⬜ not ported |
| `TestSubnetValidatorBLSKeyDiffAfterExpiry` | ⬜ not ported |
| `TestSubnetValidatorManagerAfterMultipleExpiration` | ⬜ not ported |
| `TestSubnetValidatorPopulatedToEmptyBLSKeyDiff` | ⬜ not ported |
| `TestSubnetValidatorPublicKeyDiffOnPrimaryAndSubnetReplacement` | ⬜ not ported |
| `TestSubnetValidatorRemoveAddRemoveInSingleBlock` | ⬜ not ported |
| `TestSubnetValidatorRemoveAndReplaceInSingleBlock` | ⬜ not ported |
| `TestSubnetValidatorReplacementWithUnchangedPrimaryKey` | ⬜ not ported |
| `TestSubnetValidatorSetAfterPrimaryNetworkValidatorRemoval` | ⬜ not ported |
| `TestSubnetValidatorVerifySubnetID` | ⬜ not ported |
| `TestSumWeight` | ⬜ not ported |
| `TestThrottleBlockBuildingUntilNormalOperationsStart` | ⬜ not ported |
| `TestTimestampListGenerator` | ⬜ not ported |
| `TestTrackedSubnet` | ⬜ not ported |
| `TestTransferSubnetOwnershipTx` | ⬜ not ported |
| `TestTransferSubnetOwnershipTxSerialization` | ⬜ not ported |
| `TestTransferSubnetOwnershipTxSyntacticVerify` | ⬜ not ported |
| `TestTransformSubnetTxSerialization` | ⬜ not ported |
| `TestTransformSubnetTxSyntacticVerify` | ⬜ not ported |
| `TestTxComplexity_Batch` | ⬜ not ported |
| `TestTxComplexity_Individual` | ⬜ not ported |
| `TestUnneededBuildBlock` | ⬜ not ported |
| `TestUnsignedCreateChainTxVerify` | ⬜ not ported |
| `TestUnsignedMessage` | ⬜ not ported |
| `TestUnverifiedParent` | ⬜ not ported |
| `TestUnverifiedParentPanicRegression` | ⬜ not ported |
| `TestUptimeDisallowedAfterNeverConnecting` | ⬜ not ported |
| `TestUptimeDisallowedWithRestart` | ⬜ not ported |
| `TestValidatorSetAtCacheOverwriteRegression` | ⬜ not ported |
| `TestValidatorSetRaceCondition` | ⬜ not ported |
| `TestValidatorSetReturnsCopy` | ⬜ not ported |
| `TestValidatorStakingInfo` | ⬜ not ported |
| `TestValidatorUptimes` | ⬜ not ported |
| `TestValidatorWeightDiff` | ⬜ not ported |
| `TestVerifierVisitAbortBlock` | ⬜ not ported |
| `TestVerifierVisitApricotAbortBlockUnexpectedParentState` | ⬜ not ported |
| `TestVerifierVisitApricotCommitBlockUnexpectedParentState` | ⬜ not ported |
| `TestVerifierVisitApricotStandardBlockWithProposalBlockParent` | ⬜ not ported |
| `TestVerifierVisitAtomicBlock` | ⬜ not ported |
| `TestVerifierVisitBanffAbortBlockUnexpectedParentState` | ⬜ not ported |
| `TestVerifierVisitBanffCommitBlockUnexpectedParentState` | ⬜ not ported |
| `TestVerifierVisitBanffStandardBlockWithProposalBlockParent` | ⬜ not ported |
| `TestVerifierVisitCommitBlock` | ⬜ not ported |
| `TestVerifierVisitProposalBlock` | ⬜ not ported |
| `TestVerifierVisitStandardBlock` | ⬜ not ported |
| `TestVerifyAddPermissionlessValidatorTx` | ⬜ not ported |
| `TestVerifySpendUTXOs` | ⬜ not ported |
| `TestVerifyUnverifiedParent` | ⬜ not ported |
| `TestVerifyWarpMessages` | ⬜ not ported |
| `TestVerifyWarpMessages` | ⬜ not ported |
| `TestWriteDelegatorMetadata` | ⬜ not ported |
| `TestWriteValidatorMetadata` | ⬜ not ported |

## M4.6 — codec gate (`tests/golden_codec.rs`, `tests/prop_roundtrip.rs`)

The codec gate has two layers:

1. **Round-trip property** (`prop_roundtrip::pchain_tx_roundtrip`, 1024 cases) —
   `decode(encode(x)) == x` for an arbitrary `UnsignedTx` covering **every** one
   of the 23 enum variants, plus `pchain_signed_tx_roundtrip` (signed `Tx`
   `initialize`/`parse`) and `decode_never_panics` (arbitrary bytes → `Tx::parse`
   / `Block::parse`, the stable substitute for the `decode_block_tx` cargo-fuzz
   target).
2. **Byte-exact goldens** (`golden_codec::pchain_tx_codec` /
   `golden_codec::pchain_block_hash`) — ported verbatim from the Go
   `expectedBytes` constants.

### Per-`UnsignedTx`-variant golden-vector coverage

| Variant | Byte-exact Go golden | Round-trip prop |
|---|---|---|
| `AddPermissionlessValidator` | ✅ `golden_codec` | ✅ |
| `RegisterL1Validator` | ✅ `golden_codec` | ✅ |
| `IncreaseL1ValidatorBalance` | ✅ `golden_codec` | ✅ |
| `SetL1ValidatorWeight` | ✅ `golden_codec` | ✅ |
| `DisableL1Validator` | ✅ `golden_codec` | ✅ |
| `ConvertSubnetToL1` | 🟡 na — Go vector exists (`convert_subnet_to_l1_tx_test.go`) but is large; deferred | ✅ |
| `AddValidator` | ⬜ na — Go `expectedBytes` not yet ported | ✅ |
| `AddSubnetValidator` | ⬜ na — Go `expectedBytes` not yet ported | ✅ |
| `AddDelegator` | ⬜ na — Go `expectedBytes` not yet ported | ✅ |
| `CreateChain` | ⬜ na — Go `expectedBytes` not yet ported | ✅ |
| `CreateSubnet` | ⬜ na — Go `expectedBytes` not yet ported | ✅ |
| `Import` | ⬜ na — Go `expectedBytes` not yet ported | ✅ |
| `Export` | ⬜ na — Go `expectedBytes` not yet ported | ✅ |
| `AdvanceTime` | ⬜ na — Go `expectedBytes` not yet ported | ✅ |
| `RewardValidator` | ⬜ na — Go `expectedBytes` not yet ported | ✅ |
| `RemoveSubnetValidator` | ⬜ na — Go vector exists; deferred | ✅ |
| `TransformSubnet` | ⬜ na — Go vector exists; deferred | ✅ |
| `AddPermissionlessDelegator` | ⬜ na — Go vector exists; deferred | ✅ |
| `TransferSubnetOwnership` | ⬜ na — Go vector exists; deferred | ✅ |
| `Base` | ⬜ na — Go vector exists (`base_tx_test.go`); deferred | ✅ |
| `AddAutoRenewedValidator` | ⬜ na — Go vector exists; deferred | ✅ |
| `SetAutoRenewedValidatorConfig` | ⬜ na — Go vector exists; deferred | ✅ |
| `RewardAutoRenewedValidator` | ⬜ na — Go vector exists; deferred | ✅ |

Variants without a ported byte-exact Go vector are covered by the round-trip
property (item 1), which is sufficient to catch any field-ordering / encoding
regression; the byte-exact ports are additive and tracked here for follow-up.

## M8.22 — `platform.*` JSON-RPC method inventory vs Go (`vms/platformvm/service.go`)

`PlatformVm::create_handlers` mounts the gorilla service `platform` at
extension `""` (Go `vm.go:451-466`), served through the in-process
`HttpHandler` seam by `service::RpcService` + the crate-local
`jsonrpc.rs` shim (`ava-api` is unreachable from this crate:
`ava-api → ava-config → ava-genesis → ava-platformvm` is a package cycle; the
`#[rpc_service]` macro is shared via the leaf `ava-api-macros` crate and the
dispatch core is pinned to `ava-api`'s by parity tests). The Go set is the 31
exported `Service` methods; **as of M8.23a all 31 are bridged** (method-set
parity). The remaining deferrals are reply-shape / seam refinements noted
per row below; the recorded-Go differential harness (golden vectors) is a
separate task.

### Bridged (31) — exact Go wire names

M8.22 bridged 16; **M8.23a (this task) bridged the remaining 15** + upgraded
the locally-reachable PARTIAL shapes.

| Method | Notes |
|---|---|
| `getHeight` | |
| `getProposedHeight` | justified trivial delegation: Go's body is exactly `vm.GetMinimumHeight` = the `ValidatorState::get_minimum_height` seam the service already holds |
| `getTimestamp` | |
| `getCurrentSupply` | |
| `getCurrentValidators` | PARTIAL reply shape: no delegator/uptime/owner attributes yet (needs the staker-attributes cache + owner formatting) |
| `getL1Validator` | PARTIAL reply shape: read-relevant subset (no remaining-balance/owner fields) |
| `getValidatorsAt` | `height` accepts `json.Uint64` + `"proposed"` (decode parity); `"proposed"` resolves to `MaxUint64`, which fails the height lookup until the proposer-height seam lands |
| `getFeeState` | M8.23a: `price` is now the live `gas.CalculatePrice(MinPrice, excess, K)`; `capacity`/`excess`/`price` are plain JSON numbers (Go `gas.Gas`/`gas.Price` are bare `uint64`, no marshaler) |
| `getValidatorFeeState` | M8.23a: live `price`; plain JSON numbers (same rationale) |
| `validatedBy` | |
| `validates` | |
| `getTxStatus` | accepted-state only: `Committed`/`Unknown` (the mempool/preferred-chain `Processing`/`Dropped` walk needs the builder seam) |
| `getTx` | `hex`/`hexnc` encodings; `json` (typed tx JSON) deferred |
| `getBlock` | `hex`/`hexnc`; `json` (typed block JSON) deferred |
| `getBlockByHeight` | same encoding note |
| `getStakingAssetID` | primary network = `ctx.AVAXAssetID`; a non-primary subnet surfaces Go's `failed fetching subnet transformation…: not found` (elastic-subnet transform state not ported) |
| `getBalance` | **M8.23a** — `avax.GetAllUTXOs` over the address→UTXO index (M8.23a state seam); full locktime/stakeable-lock classification; scalar AVAX fields duplicate the maps' AVAX entry (Go backwards-compat). Multi-asset/locktime parity complete |
| `getUTXOs` | **M8.23a** — local `avax.GetPaginatedUTXOs` port (ascending `(addr,utxoID)` page, `limit` clamp 1024, end-cursor). DEFERRED: cross-chain `sourceChain` atomic UTXOs (`avax.GetAtomicUTXOs`) — needs the shared-memory seam (M8); requesting a non-empty `sourceChain` returns a clear error |
| `getSubnet` | **M8.23a** — `state.GetSubnetOwner` decode (control keys/threshold/locktime) + L1-conversion (`manager*`) slot. DEFERRED: elastic-subnet `GetSubnetTransformation` (`isPermissioned`/`subnetTransformationTxID` reflect only the L1-conversion slot; transform state not ported) |
| `getSubnets` | **M8.23a** — `state.GetSubnetIDs` + per-subnet owner; primary network always included. DEFERRED: the elastic-transform branch (no transform state) |
| `sampleValidators` | **M8.23a** — weighted-without-replacement over the current validator set (`ValidatorState::get_current_validator_set`); mirrors Go's WWR weight-position draw (duplicate node ids possible, `utils.Sort`-ed). RNG is non-deterministic in Go (untestable for byte parity) |
| `getBlockchainStatus` | **M8.23a** — accepted-state walk: `Validating` (node validates the chain's subnet) / `Created` (accepted create-chain tx) / `Unknown`. DEFERRED: the chain-alias `Syncing` + preferred-but-unaccepted `Preferred` cases (need the chain registry + preferred-chain state-manager seams) |
| `getBlockchains` | **M8.23a** — `state.GetChains` over every subnet + primary, decoding each `CreateChainTx` (name/vmID) |
| `issueTx` | **M8.23a** — decode/parse fully ported; the wire handler admits through the `TxIssuer` mempool seam. DEFERRED at runtime: the production seam is a `DeferredIssuer` (the P-Chain mempool is un-shared on `PlatformVm`; shared-mempool + gossip admission is M8 node assembly). Local conformance uses a recording issuer |
| `getStake` | **M8.23a** — walks the current+pending staker sets, decodes each staker tx's `stake_outs`, sums the outputs owned by the queried addresses (no separate staker-attributes cache needed) |
| `getMinStake` | **M8.23a** — primary network from `StakingConfig` (2000/25 AVAX). DEFERRED: non-primary (elastic) subnets need the transform-subnet state (returns Go's `failed fetching subnet transformation…`); the per-network (Fuji) min-stake plumb is ava-genesis |
| `getTotalStake` | **M8.23a** — summed from `get_current_validator_set` (the `vm.Validators.TotalWeight` equivalent); `stake` is the deprecated alias of `weight` |
| `getRewardUTXOs` | **M8.23a** — `state.GetRewardUTXOs` index, encoded per `hex`/`hexnc` |
| `getAllValidatorsAt` | **M8.23a** — `get_warp_validator_sets(height)` grouped into the Go `validators.Warp` JSON shape (by compressed pubkey, sorted by uncompressed bytes); `"proposed"` resolves via `get_minimum_height` |
| `getFeeConfig` | **M8.23a** — the dynamic-fee `gas.Config` constants (`WEIGHTS`/capacity/rates/min-price/K); plain JSON numbers |
| `getValidatorFeeConfig` | **M8.23a** — the validator continuous-fee `fee.Config` (capacity/target/min-price/per-network K); plain JSON numbers |

### Acronym-method `#[rpc(name)]` overrides (exact-remainder dispatch)

`GetUTXOs`, `GetRewardUTXOs`, and `GetStakingAssetID`, `GetL1Validator` carry
`#[rpc(name = "…")]` so the snake_case ident pascalizes to the exact Go wire
name; the default pascalization (`GetUtxos`/`GetRewardUtxos`) must MISS, which
the `platform_method_set_matches_bridged` test asserts.

### Remaining deferrals (reply-shape / runtime seams, post-M8.23a)

- `getUTXOs` cross-chain `sourceChain` atomic UTXOs (shared-memory seam, M8).
- `issueTx` runtime admission (shared P-Chain mempool + gossip, M8 node assembly).
- Elastic-subnet transform state (`getSubnet`/`getSubnets`/`getMinStake`/`getStakingAssetID`).
- `getCurrentValidators`/`getL1Validator` delegator/uptime/owner attributes.
- `getTx`/`getBlock` `json` (typed) encoding.
- `getBlockchainStatus` chain-alias `Syncing` + `Preferred` (chain registry + preferred-chain state manager).
- The recorded-Go differential harness (golden wire vectors) — separate task.

Recorded transport deferral: Go wraps each handler with the `vm.metrics`
request interceptor (`vm.go:455-456`) — deferred with the proposervm M8.22
precedent.
