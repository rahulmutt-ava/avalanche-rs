// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Per-tx-type complexity computation — the `[Bandwidth, DBRead, DBWrite,
//! Compute]` 4-vector for each P-Chain tx (`vms/platformvm/txs/fee/complexity.go`).
//!
//! This module currently provides the complexity *primitives*: the intrinsic
//! component-cost constants (signature-verification compute costs, intrinsic
//! per-input/output DB reads/writes, etc.) measured on the reference AWS
//! c5.xlarge benchmarks, plus the [`base_tx_complexity`] bandwidth baseline.
//!
//! The per-tx-type complexity computation (the `txs.Visitor` that walks each
//! concrete unsigned-tx struct) is **deferred**: the tx structs are introduced
//! by task M4.3, a later wave. Once they land, this module gains a visitor that
//! sums the intrinsics below with the per-input/output/credential costs.

use crate::txs::fee::gas::Dimensions;

// Signature-verification & component costs (`complexity.go`), conservatively
// benchmarked on an AWS c5.xlarge. Units: Compute is microseconds.

/// secp256k1 signature verification compute cost (~200 µs).
pub const SECP256K1_FX_SIGNATURE_COMPUTE: u64 = 200;
/// BLS public-key aggregation compute cost (~5 µs).
pub const BLS_AGGREGATE_COMPUTE: u64 = 5;
/// BLS signature verification compute cost (~1000 µs).
pub const BLS_VERIFY_COMPUTE: u64 = 1_000;
/// BLS public-key validation compute cost (~50 µs).
pub const BLS_PUBLIC_KEY_VALIDATION_COMPUTE: u64 = 50;
/// BLS proof-of-possession verify cost: pubkey validation + signature verify.
pub const BLS_POP_VERIFY_COMPUTE: u64 = BLS_PUBLIC_KEY_VALIDATION_COMPUTE + BLS_VERIFY_COMPUTE;

/// Intrinsic DB reads per transaction input (`intrinsicInputDBRead`).
pub const INPUT_DB_READ: u64 = 1;
/// Intrinsic DB writes per transaction input (`intrinsicInputDBWrite`).
pub const INPUT_DB_WRITE: u64 = 1;
/// Intrinsic DB writes per transaction output (`intrinsicOutputDBWrite`).
pub const OUTPUT_DB_WRITE: u64 = 1;

// Bandwidth constants, in bytes, mirroring the Go wire-size accounting.
// `LongLen = 8`, `IntLen = 4`, `ShortLen = 2`, `IDLen = 32`, `NodeIDLen = 20`,
// codec `VersionSize = 2`.

const INT_LEN: u64 = 4;
const ID_LEN: u64 = 32;
const CODEC_VERSION_SIZE: u64 = 2;

/// The intrinsic bandwidth of `BaseTx` — the common header every P-Chain tx
/// carries (`IntrinsicBaseTxComplexities[Bandwidth]`).
///
/// `codecVersion + typeID + networkID + blockchainID + numOutputs + numInputs +
/// memoLen + numCredentials`.
pub const BASE_TX_BANDWIDTH: u64 = CODEC_VERSION_SIZE
    + INT_LEN // typeID
    + INT_LEN // networkID
    + ID_LEN // blockchainID
    + INT_LEN // number of outputs
    + INT_LEN // number of inputs
    + INT_LEN // length of memo
    + INT_LEN; // number of credentials

/// The intrinsic complexity of a bare `BaseTx`
/// (`IntrinsicBaseTxComplexities`): only the header bandwidth; no intrinsic DB
/// reads/writes or compute.
#[must_use]
pub fn base_tx_complexity() -> Dimensions {
    [BASE_TX_BANDWIDTH, 0, 0, 0]
}

// TODO(after M4.3): per-tx complexity wiring. Implement a `txs::Visitor` that,
// for each concrete unsigned tx (BaseTx, AddSubnetValidatorTx, CreateChainTx,
// CreateSubnetTx, ImportTx, ExportTx, the ACP-77 L1 txs, …), sums the intrinsic
// complexities above with the per-input/output/credential/signer costs, exactly
// as `complexityVisitor` does in `complexity.go`. The tx structs do not exist
// until task M4.3, so the per-tx computation is deferred to that wave.

#[cfg(test)]
mod golden {
    use super::*;

    #[test]
    fn base_tx_bandwidth() {
        // 2 + 4 + 4 + 32 + 4 + 4 + 4 + 4 = 58 bytes.
        assert_eq!(BASE_TX_BANDWIDTH, 58);
        assert_eq!(base_tx_complexity(), [58, 0, 0, 0]);
        assert_eq!(BLS_POP_VERIFY_COMPUTE, 1_050);
    }
}
