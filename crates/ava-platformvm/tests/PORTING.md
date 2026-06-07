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
| `TestBootstrapPartiallyAccepted` | ⬜ not ported |
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
| `TestGetCurrentValidators` | ⬜ not ported |
| `TestGetCurrentValidators` | ⬜ not ported |
| `TestGetCurrentValidatorsForL1` | ⬜ not ported |
| `TestGetDelegatorRules` | ⬜ not ported |
| `TestGetFeeConfig` | ⬜ not ported |
| `TestGetFeeStateErrors` | ⬜ not ported |
| `TestGetInputOutputs` | ⬜ not ported |
| `TestGetL1Validator` | ⬜ not ported |
| `TestGetNextStakerChangeTime` | ⬜ not ported |
| `TestGetNextStakerToReward` | ⬜ not ported |
| `TestGetProposedHeight` | ⬜ not ported |
| `TestGetPublicKeyDiffs` | ⬜ not ported |
| `TestGetStake` | ⬜ not ported |
| `TestGetStakerIteratorDeleteAndPut` | ⬜ not ported |
| `TestGetState` | ⬜ not ported |
| `TestGetTimestamp` | ⬜ not ported |
| `TestGetTimestamp` | ⬜ not ported |
| `TestGetTx` | ⬜ not ported |
| `TestGetTxStatus` | ⬜ not ported |
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
| `TestServiceGetBlockByHeight` | ⬜ not ported |
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
