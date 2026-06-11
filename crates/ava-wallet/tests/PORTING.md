# `ava-wallet` — Go → Rust porting matrix

Tracks coverage of Go `wallet/...` tests (specs 02 §13). Rows are seeded from
`go test -list '.*' ./wallet/...` (builder + utxo tests) against the
`../avalanchego` reference tree @ `cc3b103b91173f5e8b89b1b31aea0816766c8ada`.
Golden tx vectors under `tests/vectors/wallet/{p,x,c}.json` were emitted by the
committed in-repo Go emitters in `tests/go-oracle/` (copy into `../avalanchego`,
env-gated run, see the file headers for the exact command; `AVAX_RS_GO_COMMIT`
stamps provenance).

Legend: ⬜ not ported · 🟡 partial · ✅ ported

| Go test | Status |
|---|---|
| `TestBaseTx` (P) | ✅ `p::builder::tests::new_base_tx_bytes_match_go` (+ memo variant) |
| `TestAddPermissionlessValidatorTx` | ✅ `p::builder::tests::new_add_permissionless_validator_tx_bytes_match_go` |
| `TestAddPermissionlessDelegatorTx` | ✅ `p::builder::tests::new_add_permissionless_delegator_tx_bytes_match_go` |
| `TestAddSubnetValidatorTx` | ✅ `p::builder::tests::new_add_subnet_validator_tx_bytes_match_go` |
| `TestRemoveSubnetValidatorTx` | ✅ `p::builder::tests::new_remove_subnet_validator_tx_bytes_match_go` |
| `TestCreateSubnetTx` | ✅ `p::builder::tests::new_create_subnet_tx_bytes_match_go` |
| `TestCreateChainTx` | ✅ `p::builder::tests::new_create_chain_tx_bytes_match_go` |
| `TestTransferSubnetOwnershipTx` | ✅ `p::builder::tests::new_transfer_subnet_ownership_tx_bytes_match_go` |
| `TestImportTx` (P) | ✅ `p::builder::tests::new_import_tx_bytes_match_go` |
| `TestExportTx` (P) | ✅ `p::builder::tests::new_export_tx_bytes_match_go` |
| `TestConvertSubnetToL1Tx` | ✅ `p::builder::tests::new_convert_subnet_to_l1_tx_bytes_match_go` |
| `TestRegisterL1ValidatorTx` | ✅ `p::builder::tests::new_register_l1_validator_tx_bytes_match_go` |
| `TestSetL1ValidatorWeightTx` | ✅ `p::builder::tests::new_set_l1_validator_weight_tx_bytes_match_go` |
| `TestIncreaseL1ValidatorBalanceTx` | ✅ `p::builder::tests::new_increase_l1_validator_balance_tx_bytes_match_go` |
| `TestDisableL1ValidatorTx` | ✅ `p::builder::tests::new_disable_l1_validator_tx_bytes_match_go` |
| `TestAddAutoRenewedValidatorTx` | ✅ `p::builder::tests::new_add_auto_renewed_validator_tx_bytes_match_go` (ACP-236 upstream delta) |
| `TestSetAutoRenewedValidatorConfigTx` | ✅ `p::builder::tests::new_set_auto_renewed_validator_config_tx_bytes_match_go` |
| `TestBaseTx` (X) | ✅ `x::builder::tests::x_base_tx_bytes_match_go` (+ memo variant) |
| `TestCreateAssetTx` (X) | ✅ `x::builder::tests::x_create_asset_tx_bytes_match_go` |
| `TestImportTx` (X) | ✅ `x::builder::tests::x_import_tx_bytes_match_go` (+ AVAX<fee local top-up branch) |
| `TestExportTx` (X) | ✅ `x::builder::tests::x_export_tx_bytes_match_go` |
| `TestMintFTOperation` / `TestMintNFTOperation` / `TestMintPropertyOperation` / `TestBurnPropertyOperation` | ⬜ **DEFERRED** — X `OperationTx` needs typed fx-operation types `ava-avm` doesn't have (M5 §5.5 follow-up); signer + facade return `UnsupportedTxType` (`primary::tests::x_operation_tx_unsupported`) |
| `TestImportTx` (C) | ✅ `c::builder::tests::{c_import_x_bytes_match_go,c_import_p_bytes_match_go}` (non-AVAX UTXOs skipped) |
| `TestExportTx` (C) | ✅ `c::builder::tests::c_export_x_bytes_match_go` (C→X) |
| `TestSplitByLocktime` / `TestByAssetID` / `TestUnwrapOutput` | ✅ exercised inside `common::utxo_select` deterministic-selection tests (M8.25) |

## Facade / primary wallet (M8.27)

Go `wallet/subnet/primary` + per-chain `wallet.go`/`backend_visitor.go` are
ported as `src/{p,x,c}/wallet.rs` + `src/primary.rs`:
`issue_*_tx` = build → sign → `client.issue_tx` → await-accepted (unless
`AssumeDecided`) → `Backend::accept_tx` recording (consumed UTXOs removed,
produced UTXOs added, owners tracked); `make_wallet` mirrors Go
`MakeWallet`/`FetchState` (info + P/X/C contexts, 9 source×destination UTXO
views, owners, eth balance/nonce). Covered by `primary::tests::{issue_flow_records_in_backend,
make_wallet_fetches_state, cross_chain_export_records_into_destination_backend,
c_facade_resolves_base_fee_and_records, x_operation_tx_unsupported}`.

## Deferrals

- **Live HTTP transport**: `src/client.rs` defines the narrow client traits the
  wallet consumes (Info 2, P 7, X 5, C 3, Eth 3 methods); tests use in-memory
  mocks. The JSON-RPC-over-HTTP implementations land with the `ava-api`
  client work (M8.18/M8.22/M8.23) — the repo's deferred-live-transport
  pattern (M7.20/M7.23 precedent). UTXO paging (Go `fetchLimit=1024` loop in
  `AddAllUTXOs`) is a transport concern and lives with that client.
- **X `OperationTx`** (mint FT/NFT/property): blocked on typed fx-operation
  types in `ava-avm` (M5 §5.5 follow-up).
- Known benign divergences: C `balance()`/`nonce()` return 0 for untracked
  accounts where Go returns `database.ErrNotFound` (downstream: "insufficient
  funds" instead of "not found"); the typed per-chain UTXO store errors
  `UnknownOutputType` on cross-boundary `StakeableLock`/`SecpMint` exports Go
  would store untyped (wallet builders never produce these) — see
  `src/common/utxos.rs` module doc.
