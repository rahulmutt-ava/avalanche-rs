// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ExecutionResults` — the per-block execution artefact persisted by the SAE
//! executor, plus a height-indexed store for it (specs/11 §4.1/§7).
//!
//! Port of `vms/saevm/blocks/execution.canoto.go` / `vms/saevm/types`.

use ava_database::HeightIndex;
use ava_evm_reth::B256;
use ava_saevm_proxytime::Time;
use ava_vm::components::gas::Price;

/// Byte length of the fixed-layout [`ExecutionResults`] blob:
/// `gas_time` (24) + `base_fee` (8) + `receipt_root` (32) + `post_state_root`
/// (32).
pub const EXECUTION_RESULTS_LEN: usize = 24 + 8 + 32 + 32;

// Field offsets within the encoded blob.
const GAS_TIME_END: usize = 24;
const BASE_FEE_END: usize = 32;
const RECEIPT_ROOT_END: usize = 64;
const POST_STATE_ROOT_END: usize = 96;

/// The result of executing one SAE block, persisted so executed artefacts
/// survive a restart independently of the execution-trie commit cadence
/// (specs/11 §7).
///
/// > **AS-BUILT (M7.8).** specs/11 §4.1 calls the persisted form a "canoto"
/// > blob. There is no canoto codec in this Rust workspace (it uses the
/// > hand-written `ava-codec` linear codec); this type instead uses a
/// > deterministic fixed-layout big-endian encoding (see [`Self::encode`]).
/// > Exact Go-canoto byte parity is a differential-milestone concern (M7.29),
/// > not a correctness requirement for the persisted-and-reloaded round-trip.
///
/// Mirrors `vms/saevm/blocks` `executionResults`.
#[derive(Clone, Debug)]
pub struct ExecutionResults {
    /// The block's gas-clock instant (proxy-time measured in gas).
    pub gas_time: Time<u64>,
    /// The base fee (gas price) that applied during execution.
    pub base_fee: Price,
    /// The block's receipts-trie root.
    pub receipt_root: B256,
    /// The post-execution state-trie root.
    pub post_state_root: B256,
}

/// Failure decoding an [`ExecutionResults`] blob.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    /// The blob was not exactly [`EXECUTION_RESULTS_LEN`] bytes.
    #[error("execution-results blob must be {EXECUTION_RESULTS_LEN} bytes, got {0}")]
    WrongLength(usize),
    /// The embedded gas-clock instant failed to decode.
    #[error("gas-time decode: {0}")]
    GasTime(&'static str),
}

impl ExecutionResults {
    /// Encodes to the fixed 96-byte layout: big-endian `gas_time`
    /// (`[seconds, fraction, hertz]`, 24 bytes) ++ big-endian `base_fee`
    /// (8 bytes) ++ `receipt_root` (32 bytes) ++ `post_state_root` (32 bytes).
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(EXECUTION_RESULTS_LEN);
        out.extend_from_slice(&self.gas_time.encode());
        out.extend_from_slice(&self.base_fee.0.to_be_bytes());
        out.extend_from_slice(self.receipt_root.as_slice());
        out.extend_from_slice(self.post_state_root.as_slice());
        out
    }

    /// Decodes the fixed-layout blob produced by [`Self::encode`].
    ///
    /// # Errors
    /// [`DecodeError::WrongLength`] if `bytes` is not exactly
    /// [`EXECUTION_RESULTS_LEN`]; [`DecodeError::GasTime`] if the embedded
    /// gas-clock instant is malformed.
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() != EXECUTION_RESULTS_LEN {
            return Err(DecodeError::WrongLength(bytes.len()));
        }
        let gas_time = Time::<u64>::decode(&bytes[..GAS_TIME_END]).map_err(DecodeError::GasTime)?;

        // Each slice below is a compile-time-fixed window of a length-checked
        // buffer, so the `try_into` conversions cannot fail.
        let base_fee_bytes: [u8; 8] = bytes[GAS_TIME_END..BASE_FEE_END]
            .try_into()
            .map_err(|_| DecodeError::WrongLength(bytes.len()))?;
        let base_fee = Price(u64::from_be_bytes(base_fee_bytes));

        let receipt_arr: [u8; 32] = bytes[BASE_FEE_END..RECEIPT_ROOT_END]
            .try_into()
            .map_err(|_| DecodeError::WrongLength(bytes.len()))?;
        let post_arr: [u8; 32] = bytes[RECEIPT_ROOT_END..POST_STATE_ROOT_END]
            .try_into()
            .map_err(|_| DecodeError::WrongLength(bytes.len()))?;

        Ok(Self {
            gas_time,
            base_fee,
            receipt_root: B256::from(receipt_arr),
            post_state_root: B256::from(post_arr),
        })
    }
}

impl PartialEq for ExecutionResults {
    fn eq(&self, other: &Self) -> bool {
        // `Time<u64>` has no `PartialEq`; compare it by its canonical encoding.
        self.gas_time.encode() == other.gas_time.encode()
            && self.base_fee == other.base_fee
            && self.receipt_root == other.receipt_root
            && self.post_state_root == other.post_state_root
    }
}

impl Eq for ExecutionResults {}

/// Failure reading or writing the height-indexed results store.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// The underlying [`HeightIndex`] backend failed.
    #[error("results db: {0}")]
    Database(#[from] ava_database::Error),
    /// A stored blob failed to decode.
    #[error("results db decode: {0}")]
    Decode(#[from] DecodeError),
}

/// A height-indexed store of [`ExecutionResults`], backed by any
/// [`HeightIndex`] (specs/11 §7).
pub struct ExecutionResultsDb<H: HeightIndex> {
    inner: H,
}

impl<H: HeightIndex> ExecutionResultsDb<H> {
    /// Wraps a [`HeightIndex`] backend as an execution-results store.
    pub fn new(inner: H) -> Self {
        Self { inner }
    }

    /// Persists `results` at `height`.
    ///
    /// # Errors
    /// Propagates the backend [`ava_database::Error`].
    pub fn put(&self, height: u64, results: &ExecutionResults) -> Result<(), DbError> {
        self.inner.put(height, &results.encode())?;
        Ok(())
    }

    /// Reads the results stored at `height`.
    ///
    /// # Errors
    /// [`DbError::Database`] (e.g. [`ava_database::Error::NotFound`]) if the
    /// height is absent, or [`DbError::Decode`] if the stored blob is corrupt.
    pub fn get(&self, height: u64) -> Result<ExecutionResults, DbError> {
        let bytes = self.inner.get(height)?;
        Ok(ExecutionResults::decode(&bytes)?)
    }

    /// Reports whether results are stored at `height`.
    ///
    /// # Errors
    /// Propagates the backend [`ava_database::Error`].
    pub fn has(&self, height: u64) -> Result<bool, DbError> {
        Ok(self.inner.has(height)?)
    }
}
