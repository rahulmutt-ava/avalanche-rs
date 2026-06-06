# PORTING.md — `ava-merkledb`

Tracks parity of this crate against its avalanchego source packages: the trie
implementation `x/merkledb` and the state-sync packages `database/merkle/sync`
+ `database/merkle/firewood/syncer`. One row per upstream Go test; status is one
of `todo` / `wip` / `ported` / `na`. The milestone exit gate (M1.26) requires no
`wip` rows for shipped surfaces. See `specs/02-testing-strategy.md` §10.1.

Seeded from `go test -list '.*'` over `./x/merkledb/`, `./database/merkle/sync/`,
and `./database/merkle/firewood/syncer/` at avalanchego rev `fb174e8925`.

> Note: in this avalanchego rev the path-based trie lives in `x/merkledb` (the
> task brief's `database/merkle/` is the *new* firewood-backed merkle home; the
> classic trie ported here is still `x/merkledb`). The merkle sync protocol is
> at `database/merkle/sync`.

Owning tasks: M1.12 (Key/Path), M1.13 (node + codec), M1.14 (hashing +
`golden::merkledb_root`), M1.15 (View/history/node stores), M1.16
(`prop::merkle_order_independent_root`), M1.17 (single proof), M1.18
(range/change proof), M1.19 (SyncDb + Syncer + work-heap), M1.20 (Firewood SHA),
M1.21 (Firewood ethhash), M1.25 (fuzz).

## `x/merkledb` — Key / Path (M1.12)

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `TestEncodeKey` / `TestDecodeKey` | `tests/golden_key.rs` `golden::key_pack` (Go-extracted) + `src/key.rs` `#[cfg(test)]` | ported |
| `TestCodecDecodeKeyLengthOverflowRegression` | `src/codec.rs` `#[cfg(test)]` length-overflow rejection | ported |
| `TestBranchFactor_Valid` | `src/key.rs` `#[cfg(test)]` `BranchFactor` validation | ported |
| `TestHasPartialByte` | `src/key.rs` `#[cfg(test)]` partial-byte | ported |
| `Test_Key_Has_Prefix` | `src/key.rs` `#[cfg(test)]` `has_prefix` | ported |
| `Test_Key_Skip` | `src/key.rs` `#[cfg(test)]` `skip` | ported |
| `Test_Key_Take` | `src/key.rs` `#[cfg(test)]` `take` | ported |
| `Test_Key_Token` | `src/key.rs` `#[cfg(test)]` `token` | ported |
| `Test_Key_Append` | `src/key.rs` `#[cfg(test)]` `append` | ported |
| `Test_Key_AppendExtend` | `src/key.rs` `#[cfg(test)]` `append_extend` | ported |
| `TestKeyBytesNeeded` | `src/key.rs` `#[cfg(test)]` bytes-needed | ported |
| `TestShiftCopy` | `src/key.rs` `#[cfg(test)]` shift-copy | ported |
| `TestUintSize` | `src/codec.rs` `#[cfg(test)]` varint size | ported |
| `FuzzCodecKey` | `tests/prop_fuzz_smoke.rs` `prop::node_codec_never_panics` + fuzz `node_codec` target | ported |
| `FuzzKeyDoubleExtend_Tokens` | proptest key strategies in `tests/prop_merkle.rs` | ported |
| `FuzzKeyDoubleExtend_Any` | proptest key strategies in `tests/prop_merkle.rs` | ported |
| `FuzzKeySkip` | proptest key strategies in `tests/prop_merkle.rs` | ported |
| `FuzzKeyTake` | proptest key strategies in `tests/prop_merkle.rs` | ported |

## `x/merkledb` — node model + on-disk codec (M1.13)

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `TestEncodeDBNode` | `tests/golden_node_codec.rs` `golden::node_codec_encode` (Go-extracted) | ported |
| `TestDecodeDBNode` | `tests/golden_node_codec.rs` `golden::node_codec_decode_rejects` | ported |
| `TestCodecDecodeDBNode_TooShort` | `golden::node_codec_decode_rejects` (too-short case) | ported |
| `Test_Node_Marshal` | `src/node.rs` `#[cfg(test)]` marshal round-trip | ported |
| `Test_Node_Marshal_Errors` | `golden::node_codec_decode_rejects` (errChildIndexTooLarge / errTooManyChildren) | ported |
| `FuzzCodecBool` | `tests/prop_fuzz_smoke.rs` `prop::node_codec_never_panics` | ported |
| `FuzzCodecInt` | `tests/prop_fuzz_smoke.rs` `prop::node_codec_never_panics` | ported |
| `FuzzCodecDBNodeCanonical` | fuzz `node_codec` target + `prop::node_codec_never_panics` | ported |
| `FuzzCodecDBNodeDeterministic` | fuzz `node_codec` target + `prop::node_codec_never_panics` | ported |

## `x/merkledb` — hashing + root (M1.14)

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `Test_SHA256_HashNode` | `tests/golden_root.rs` `golden::merkledb_root` (EMPTY→single→multi over BranchFactor 256/16/2) | ported |
| `Fuzz_SHA256_HashNode` | `tests/prop_merkle.rs` `prop::merkle_order_independent_root` | ported |
| `Benchmark_SHA256_HashNode` | n/a (benchmark) | na — perf bench |

## `x/merkledb` — DB / View / Trie / history / node stores (M1.15)

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `Test_MerkleDB_Get_Safety` | `tests/view.rs` + `src/db.rs` `#[cfg(test)]` get-memory-safety | ported |
| `Test_MerkleDB_GetValues_Safety` | `src/db.rs` `#[cfg(test)]` get-values safety | ported |
| `Test_MerkleDB_DB_Interface` | `tests/view.rs` `view_layering_equals_direct` + DB ops | ported |
| `Test_MerkleDB_DB_Load_Root_From_DB` | `tests/view.rs` `clean_shutdown_rebuild` (load committed root) | ported |
| `Test_MerkleDB_DB_Rebuild` | `tests/view.rs` `clean_shutdown_rebuild` (idempotent rebuild, 27 §4.1) | ported |
| `Test_MerkleDB_Failed_Batch_Commit` | `src/db.rs` `#[cfg(test)]` failed-commit rollback | ported |
| `Test_MerkleDB_Value_Cache` | `src/node_store.rs` `#[cfg(test)]` value-node cache | ported |
| `Test_MerkleDB_Invalidate_Siblings_On_Commit` | `tests/view.rs` `commit_invalidates_siblings` | ported |
| `Test_MerkleDB_InsertNil` | `src/db.rs` `#[cfg(test)]` insert-nil ⇔ empty value | ported |
| `Test_MerkleDB_HealthCheck` | `src/db.rs` `#[cfg(test)]` health-check | ported |
| `Test_MerkleDB_GetValues` | `src/db.rs` `#[cfg(test)]` get-values | ported |
| `TestDatabaseNewUntrackedView` | `tests/view.rs` view-layering | ported |
| `TestDatabaseNewViewFromBatchOpsTracked` | `src/view.rs` `#[cfg(test)]` view-from-batch-ops | ported |
| `TestDatabaseCommitChanges` | `tests/view.rs` `view_layering_equals_direct` | ported |
| `TestDatabaseInvalidateChildrenExcept` | `tests/view.rs` `commit_invalidates_siblings` | ported |
| `TestMerkleDBClear` | `src/db.rs` `#[cfg(test)]` clear | ported |
| `Test_MerkleDB_Random_Insert_Ordering` | `tests/prop_merkle.rs` `prop::merkle_order_independent_root` | ported |
| `TestCrashRecovery` | `tests/view.rs` `clean_shutdown_rebuild` (27 §4.1) | ported |
| `Test_History_Simple` | `src/history.rs` `#[cfg(test)]` history record/lookup | ported |
| `Test_History_Large` | `src/history.rs` `#[cfg(test)]` large history | ported |
| `Test_History_Bad_GetValueChanges_Input` | `src/history.rs` `#[cfg(test)]` bad-input rejection | ported |
| `Test_History_Trigger_History_Queue_Looping` | `src/history.rs` `#[cfg(test)]` ring-buffer looping | ported |
| `Test_History_Values_Lookup_Over_Queue_Break` | `src/history.rs` `#[cfg(test)]` lookup across ring boundary | ported |
| `Test_History_RepeatedRoot` | `src/history.rs` `#[cfg(test)]` repeated-root | ported |
| `Test_History_ExcessDeletes` | `src/history.rs` `#[cfg(test)]` excess-deletes | ported |
| `Test_History_DontIncludeAllNodes` | `src/history.rs` `#[cfg(test)]` partial node set | ported |
| `Test_History_Branching2Nodes` | `src/history.rs` `#[cfg(test)]` 2-node branching | ported |
| `Test_History_Branching3Nodes` | `src/history.rs` `#[cfg(test)]` 3-node branching | ported |
| `Test_History_MaxLength` | `src/history.rs` `#[cfg(test)]` bounded max-length | ported |
| `Test_Change_List` | `src/history.rs` `#[cfg(test)]` change-list | ported |
| `TestHistoryRecord` | `src/history.rs` `#[cfg(test)]` record | ported |
| `TestHistoryKeyChangeRollback` | `src/history.rs` `#[cfg(test)]` key-change rollback | ported |
| `Test_IntermediateNodeDB` | `src/node_store.rs` `#[cfg(test)]` intermediate-node store (§10.8 prefix) | ported |
| `Test_IntermediateNodeDB_ConstructDBKey_DirtyBuffer` | `src/node_store.rs` `#[cfg(test)]` construct-db-key dirty buffer | ported |
| `TestIntermediateNodeDBClear` | `src/node_store.rs` `#[cfg(test)]` clear | ported |
| `TestIntermediateNodeDBDeleteEmptyKey` | `src/node_store.rs` `#[cfg(test)]` delete-empty-key | ported |
| `TestValueNodeDB` | `src/node_store.rs` `#[cfg(test)]` value-node store | ported |
| `TestValueNodeDBIterator` | `src/node_store.rs` `#[cfg(test)]` value-node iterator | ported |
| `TestValueNodeDBClear` | `src/node_store.rs` `#[cfg(test)]` value-node clear | ported |
| `Test_Trie_ViewOnCommittedView` | `src/trie.rs` `#[cfg(test)]` view-on-committed-view | ported |
| `Test_Trie_WriteToDB` | `src/trie.rs` `#[cfg(test)]` write-to-db | ported |
| `Test_Trie_InsertAndRetrieve` | `src/trie.rs` `#[cfg(test)]` insert/retrieve | ported |
| `Test_Trie_Overwrite` | `src/trie.rs` `#[cfg(test)]` overwrite | ported |
| `Test_Trie_Delete` | `src/trie.rs` `#[cfg(test)]` delete | ported |
| `Test_Trie_DeleteMissingKey` | `src/trie.rs` `#[cfg(test)]` delete-missing | ported |
| `Test_Trie_ExpandOnKeyPath` | `src/trie.rs` `#[cfg(test)]` expand-on-key-path | ported |
| `Test_Trie_CompressedKeys` | `src/trie.rs` `#[cfg(test)]` compressed-keys | ported |
| `Test_Trie_SplitBranch` | `src/trie.rs` `#[cfg(test)]` split-branch | ported |
| `Test_Trie_HashCountOnBranch` | `tests/prop_merkle.rs` order-independent root (hash equivalence) | ported |
| `Test_Trie_HashCountOnDelete` | `tests/prop_merkle.rs` order-independent root (hash equivalence) | ported |
| `Test_Trie_NoExistingResidual` | `src/trie.rs` `#[cfg(test)]` no-residual | ported |
| `Test_Trie_BatchApply` | `src/trie.rs` `#[cfg(test)]` batch-apply | ported |
| `Test_Trie_ChainDeletion` | `src/trie.rs` `#[cfg(test)]` chain-deletion | ported |
| `Test_Trie_Invalidate_Siblings_On_Commit` | `tests/view.rs` `commit_invalidates_siblings` | ported |
| `Test_Trie_NodeCollapse` | `src/trie.rs` `#[cfg(test)]` node-collapse | ported |
| `Test_Trie_MultipleStates` | `src/trie.rs` `#[cfg(test)]` multiple-states | ported |
| `Test_Trie_ConcurrentNewViewAndCommit` | `tests/view.rs` `commit_invalidates_siblings` + Arc-linked validity | ported |
| `NewViewOnCommittedView` / `Test_View_NewView` | `tests/view.rs` `view_layering_equals_direct` | ported |
| `TestViewInvalidate` | `tests/view.rs` `commit_invalidates_siblings` | ported |
| `TestTrieCommitToDBInvalid` | `tests/view.rs` `commit_requires_db_parent` | ported |
| `TestTrieCommitToDBValid` | `tests/view.rs` `view_layering_equals_direct` (commit-to-db) | ported |
| `Test_View_Iterator` | `src/view.rs` `#[cfg(test)]` iterator | ported |
| `Test_View_Iterator_DBClosed` | `src/view.rs` `#[cfg(test)]` iterator-after-close | ported |
| `Test_View_IteratorStart` | `src/view.rs` `#[cfg(test)]` iterator-start | ported |
| `Test_View_IteratorPrefix` | `src/view.rs` `#[cfg(test)]` iterator-prefix | ported |
| `Test_View_IteratorStartPrefix` | `src/view.rs` `#[cfg(test)]` iterator-start-prefix | ported |
| `Test_View_Iterator_Random` | `src/view.rs` `#[cfg(test)]` iterator-random + oracle | ported |
| `Test_GetValue_Safety` | `src/db.rs` `#[cfg(test)]` get-value safety | ported |
| `Test_GetValues_Safety` | `src/db.rs` `#[cfg(test)]` get-values safety | ported |
| `TestVisitPathToKey` | `src/trie.rs` `#[cfg(test)]` visit-path-to-key | ported |
| `Test_HashChangedNodes` | covered by root hashing in `golden::merkledb_root` + order-independent root | ported |
| `FuzzMerkleDBEmptyRandomizedActions` | `tests/prop_fuzz_smoke.rs` `prop::fuzz_op_stream_smoke` + fuzz `op_stream` target | ported |
| `FuzzMerkleDBInitialValuesRandomizedActions` | `tests/prop_fuzz_smoke.rs` `prop::fuzz_op_stream_smoke` + fuzz `op_stream` target | ported |
| `FuzzIntermediateNodeDBConstructDBKey` | `src/node_store.rs` `#[cfg(test)]` construct-db-key (fuzzed in op-stream) | ported |
| `Benchmark*` (BytesPool/EncodeDBNode/DecodeDBNode/EncodeKey/DecodeKey/EncodeUint/MerkleDB_DBInterface/CommitView/Iteration/SHA256_HashNode/IntermediateNodeDB_ConstructDBKey/RangeProofs/ChangeProofs/HashChangedNodes/View_NewIteratorWithStartAndPrefix/WaitGroup_*) | n/a (benchmarks) | na — perf benches |

## `x/merkledb` — cache helpers (M1.15 internals)

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `TestNewOnEvictCache` | LRU caches via `lru` crate; behavior covered by `Test_MerkleDB_Value_Cache` counterpart | na — Go's bespoke `onEvictCache` replaced by the `lru` crate (00 §4); eviction behavior exercised through node-store tests |
| `TestOnEvictCacheNoOnEvictionError` | (see above) | na — bespoke cache replaced by `lru` |
| `TestOnEvictCacheOnEvictionError` | (see above) | na — bespoke cache replaced by `lru` |

## `x/merkledb` — single / range / change proofs (M1.17, M1.18)

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `Test_Proof_Inclusion` | `tests/golden_proof.rs` `golden::merkledb_proof` (Go-extracted) | ported |
| `Test_Proof_Exclusion_Happy_Path` | `golden::merkledb_proof` (exclusion case) | ported |
| `Test_Proof_Exclusion_Has_Proof_Value` | `golden::merkledb_proof` + `src/proof.rs` `#[cfg(test)]` | ported |
| `Test_Proof_Empty` | `src/proof.rs` `#[cfg(test)]` empty-proof rejection | ported |
| `Test_Proof_Invalid_Proof` | `tests/prop_proof.rs` `proof_verify_accepts_valid_rejects_tampered` | ported |
| `Test_Proof_ValueOrHashMatches` | `src/proof.rs` `#[cfg(test)]` value-or-hash match | ported |
| `Test_Proof_Path` | `src/proof.rs` `#[cfg(test)]` proof-path | ported |
| `Test_Server_GetRangeProof` | `tests/golden_range_proof.rs` `golden::range_proof` + `src/sync` server | ported |
| `Test_Server_GetChangeProof` | `src/proof.rs` / `src/sync` `#[cfg(test)]` change-proof server | ported |
| `Test_RangeProof` | `tests/golden_range_proof.rs` `golden::range_proof` (Go-extracted) | ported |
| `Test_RangeProof_Extra_Value` | `tests/prop_proof.rs` tamper rejection | ported |
| `Test_RangeProof_Verify_Bad_Data` | `tests/prop_proof.rs` `proof_verify_accepts_valid_rejects_tampered` | ported |
| `Test_RangeProof_MaxLength` | `src/proof.rs` `#[cfg(test)]` max-length bound | ported |
| `Test_RangeProof_Syntactic_Verify` | `src/proof.rs` `#[cfg(test)]` syntactic verify | ported |
| `Test_RangeProof_BadBounds` | `src/proof.rs` `#[cfg(test)]` bad-bounds | ported |
| `Test_RangeProof_NilStart` | `src/proof.rs` `#[cfg(test)]` nil-start | ported |
| `Test_RangeProof_NilEnd` | `src/proof.rs` `#[cfg(test)]` nil-end | ported |
| `Test_RangeProof_EmptyValues` | `src/proof.rs` `#[cfg(test)]` empty-values | ported |
| `Test_ChangeProof_Missing_History_For_EndRoot` | `src/sync` `#[cfg(test)]` InsufficientHistory / NoEndRoot | ported |
| `Test_ChangeProof_BadBounds` | `src/proof.rs` `#[cfg(test)]` change-proof bad-bounds | ported |
| `Test_ChangeProof_Verify` | `src/proof.rs` `#[cfg(test)]` change-proof verify | ported |
| `Test_ChangeProof_Verify_Bad_Data` | `tests/prop_proof.rs` tamper rejection | ported |
| `Test_ChangeProof_Syntactic_Verify` | `src/proof.rs` `#[cfg(test)]` change-proof syntactic verify | ported |
| `TestVerifyKeyValues` | `src/proof.rs` `#[cfg(test)]` verify-key-values | ported |
| `TestVerifyProofPath` | `src/proof.rs` `#[cfg(test)]` verify-proof-path | ported |
| `TestProofNodeUnmarshalProtoInvalidChildBytes` | `src/proof.rs` `#[cfg(test)]` invalid-child-bytes decode reject | ported |
| `TestProofNodeUnmarshalProtoInvalidChildIndex` | `src/proof.rs` `#[cfg(test)]` invalid-child-index decode reject | ported |
| `TestProofNodeUnmarshalProtoMissingFields` | `src/proof.rs` `#[cfg(test)]` missing-fields decode reject | ported |
| `FuzzProofNodeProtoMarshalUnmarshal` | `tests/prop_proof.rs` proof round-trip proptest | ported |
| `FuzzRangeProofProtoMarshalUnmarshal` | `tests/prop_proof.rs` range-proof round-trip proptest | ported |
| `FuzzChangeProofProtoMarshalUnmarshal` | `tests/prop_proof.rs` change-proof round-trip proptest | ported |
| `FuzzRangeProofInvariants` | `tests/prop_proof.rs` `proof_verify_accepts_valid_rejects_tampered` | ported |
| `FuzzProofVerification` | `tests/prop_proof.rs` `proof_verify_accepts_valid_rejects_tampered` | ported |
| `FuzzChangeProofVerification` | `tests/prop_proof.rs` `proof_verify_accepts_valid_rejects_tampered` | ported |

## `x/merkledb` — metrics (M1.15 instrumentation)

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `Test_Metrics_Basic_Usage` | merkledb metrics are exposed via Prometheus registry | na — merkledb-internal metric wiring deferred to node observability (ava-node); not on a byte-exact surface |
| `Test_Metrics_Initialize` | (see above) | na — metrics wiring deferred to ava-node |

## `database/merkle/sync` — state-sync protocol + work-heap (M1.19)

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `Test_Sync_RangeProofRequest` | `tests/sync_roundtrip.rs` `golden::sync_proof_wire` (Go-extracted frames) + `prop::sync_proof_roundtrip` | ported |
| `Test_Sync_ChangeProofRequest` | `tests/sync_roundtrip.rs` `prop::sync_proof_roundtrip` (change→range fallback) | ported |
| `Test_Sync_BusyContextCancellation` | `src/sync` `#[cfg(test)]` Syncer cancellation (tokio `Notify`/bounded task set) | ported |
| `Test_Midpoint` | `src/sync` `#[cfg(test)]` range midpoint | ported |
| `Test_WorkHeap_Insert_GetWork` | `tests/prop_workheap.rs` `prop::workheap_invariants` + `src/sync` `#[cfg(test)]` | ported |
| `Test_WorkHeap_remove` | `src/sync` `#[cfg(test)]` work-heap remove | ported |
| `Test_WorkHeap_Merge_Insert` | `src/sync` `#[cfg(test)]` work-heap merge/coalesce | ported |
| `TestWorkHeapMergeInsertRandom` | `tests/prop_workheap.rs` `prop::workheap_invariants` | ported |
| `TestWorkHeapStatus` | `src/sync` `#[cfg(test)]` work-heap status/priority | ported |

## `x/merkledb` — Syncer integration + find-next-key (M1.19)

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `Test_Creation` | `src/sync` `#[cfg(test)]` Syncer creation | ported |
| `Test_Sync_Result_Correct_Root` | `tests/sync_roundtrip.rs` `prop::sync_proof_roundtrip` (final root == target) | ported |
| `Test_Sync_Result_Correct_Root_With_Sync_Restart` | `src/sync` `#[cfg(test)]` restart path | ported |
| `Test_Sync_Result_Correct_Root_Update_Root_During` | `tests/sync_roundtrip.rs` mid-sync `update_sync_target` → final root == new target | ported |
| `Test_Sync_UpdateSyncTarget` | `src/sync` `#[cfg(test)]` `update_sync_target` re-queue | ported |
| `Test_FindNextKey_InSync` | `src/sync` `#[cfg(test)]` find-next-key in-sync | ported |
| `Test_FindNextKey_Deleted` | `src/sync` `#[cfg(test)]` find-next-key deleted | ported |
| `Test_FindNextKey_BranchInLocal` | `src/sync` `#[cfg(test)]` branch-in-local | ported |
| `Test_FindNextKey_BranchInReceived` | `src/sync` `#[cfg(test)]` branch-in-received | ported |
| `Test_FindNextKey_ExtraValues` | `src/sync` `#[cfg(test)]` extra-values | ported |
| `Test_FindNextKey_DifferentChild` | `src/sync` `#[cfg(test)]` different-child | ported |
| `TestFindNextKeyRandom` | `tests/sync_roundtrip.rs` `prop::sync_proof_roundtrip` | ported |
| `TestGetRangeProofAtRootEmptyRootID` | `src/sync` `#[cfg(test)]` empty-root-id range proof | ported |
| `TestGetChangeProofEmptyRootID` | `src/sync` `#[cfg(test)]` empty-root-id change proof | ported |

## `database/merkle/firewood/syncer` — Firewood-backed sync (M1.20)

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `Test_Firewood_Sync` | `src/firewood` + `src/sync` `SyncDb for FirewoodDb` (native `FrozenRangeProof`/`FrozenChangeProof`); `tests/firewood_sha.rs` | ported |
| `Test_Firewood_Sync_UpdateSyncTarget` | `tests/sync_roundtrip.rs` mid-sync `update_sync_target` (protocol backend-agnostic, exercised over firewood `SyncDb`) | ported |

## Firewood SHA + ethhash (M1.20, M1.21)

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| firewood propose/commit/revision (firewood crate v0.5.0) | `tests/firewood_sha.rs` `firewood_propose_commit_roundtrip` + `firewood_reopen_with_small_revision_window` | ported |
| firewood ethhash EVM root (`firewood-go-ethhash/ffi v0.5.0`) | `tests/golden_firewood_ethhash.rs` `firewood_ethhash_root` (Go-extracted, root `eb8b07d6…`) | ported |
| ethhash empty-trie root == `types.EmptyRootHash` | `tests/golden_firewood_ethhash.rs` `firewood_ethhash_empty_root_is_eth_empty_trie` (`0x56e81f17…`) | ported |

## Notes / deviations

- **Path-based trie source is `x/merkledb`**, not `database/merkle/` (the latter
  is the firewood-backed merkle home in this rev). Sync protocol is
  `database/merkle/sync`; firewood-backed sync is
  `database/merkle/firewood/syncer`.
- **`TestNewOnEvictCache` family is `na`:** Go's bespoke `onEvictCache` is
  replaced by the `lru` crate (00 §4 — no second crate for a covered job);
  eviction behavior is exercised through the node-store / value-cache tests.
- **`Test_Metrics_*` are `na`:** merkledb-internal Prometheus wiring is deferred
  to node observability (`ava-node`) and sits on no byte-exact surface.
- **`Benchmark*` rows are `na`** — perf benches, not parity assertions
  (02 §10.1).
- **Fuzz targets** (`FuzzMerkleDB*`, `FuzzCodec*`, `FuzzKey*`, `Fuzz*Proof*`)
  map to the standalone `crates/ava-merkledb/fuzz/` cargo-fuzz targets
  (`op_stream`, `node_codec`) plus the **stable** `prop_fuzz_smoke` proptest
  harness that runs in `cargo nextest` today; the instrumented fuzz run needs
  the nightly toolchain (02 §8).
- **Proof proto encode** is a hand-rolled byte-exact protobuf encoder
  (BTreeMap-ascending `ProofNode.children`, Go `Deterministic:true` parity,
  15 §3.10); prost types are used only to decode peer responses.
- **Firewood hashing mode is a global compile-time switch** (`firewood`/SHA vs
  `firewood-ethhash`/Keccak features are mutually exclusive per build, 04 §4.1).
