// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! C-Chain block **wire format** — `decode_ava_evm_block` / `assemble_ava_block`
//! (spec 10 §9.3 + §6.2, G0/G6).
//!
//! coreth does **not** encode a plain Ethereum block. Two libevm-specific
//! deviations make the C-Chain block bytes (and therefore the block ID) differ
//! from a stock alloy/geth block, and both are consensus-critical (block IDs
//! must match Go nodes byte-for-byte — overview compatibility table):
//!
//! 1. **Header extras** (coreth `plugin/evm/customtypes/header_ext.go` +
//!    `gen_header_serializable_rlp.go`). After the 15 standard Ethereum header
//!    fields, coreth appends `ExtDataHash` (**always present**, field 16), then
//!    an *optional tail* — `BaseFee` (AP3), `ExtDataGasUsed`/`BlockGasCost` (AP4),
//!    `BlobGasUsed`/`ExcessBlobGas` (EIP-4844), `ParentBeaconRoot` (EIP-4788),
//!    `TimeMilliseconds`/`MinDelayExcess` (Granite) — included with the standard
//!    RLP-optional discipline ("any later field present ⇒ all earlier present").
//! 2. **Block body shape** (coreth `block_ext.go` `BlockRLPFieldsForEncoding`).
//!    The geth `Withdrawals` field is replaced by two Avalanche fields, so the
//!    block list is `[Header, Txs, Uncles, Version(u32), ExtData(bytes)]`.
//!    `ExtData` carries the atomic txs (post-AP5: the AP5 *batch* encoding
//!    `atomic.Codec.Marshal(0, []*Tx{...})`; empty otherwise — §6.2), and is the
//!    pre-image of `ExtDataHash` (`keccak256(rlp(ExtData))`, or `EmptyExtDataHash`
//!    when empty).
//!
//! The block **ID/hash** is `keccak256(header RLP)` (coreth `RLPHash(header)`),
//! computed over the coreth header layout above — not the standard alloy header.
//!
//! This module hand-rolls that RLP through the [`ava_evm_reth`] facade
//! (`RlpListHeader` = `alloy_rlp::Header`, the list-framing primitive) so the
//! crate never names `alloy_rlp` directly (G0).

use std::sync::Arc;

use ava_evm_reth::{
    Address, B256, Bytes, ConsensusTx as _, Decodable2718, EMPTY_OMMER_ROOT_HASH, EthReceipt,
    Header, RLP_EMPTY_STRING_CODE, RecoveredTx, RlpDecodable, RlpEncodable, RlpError,
    RlpListHeader, SignerRecoverable, State, StateBuilder, StateProviderDatabase,
    TransactionSigned, TxHashRef as _, Typed2718 as _, U256, calculate_transaction_root, keccak256,
};

use crate::atomic::backend::AtomicBackend;
use crate::atomic::tx::{Tx as AtomicTx, codec as atomic_codec};
use crate::canonical::CanonicalStore;
use crate::chainspec::{AvaChainSpec, AvaPhase};
use crate::error::{Error, Result};
use crate::evmconfig::{AvaEvmConfig, AvaExecCtx, NoopPreHook};
use crate::feerules::acp176;
use crate::feerules::window;
use crate::precompile::rewardmanager::BLACKHOLE_ADDRESS;
use crate::precompile::warp::{WarpBackend, WarpLog, WarpPrecompile, handle_precompile_accept};
use crate::receipts::{AcceptedTxIndex, TxReceiptRecord, encode_block_receipts};
use crate::state::{FirewoodStateProvider, FirewoodStateView};

/// coreth `plugin/evm/upgrade/ap0/params.go` `MinGasPrice` — 470 gwei, the
/// minimum tx gas price enforced pre-ApricotPhase1 (`wrapped_block.go:460-465`).
const AP0_MIN_GAS_PRICE: u128 = 470_000_000_000;

/// coreth `plugin/evm/upgrade/ap1/params.go` `MinGasPrice` — 225 gwei, the
/// minimum tx gas price enforced pre-ApricotPhase3 (`wrapped_block.go:466-472`).
const AP1_MIN_GAS_PRICE: u128 = 225_000_000_000;

/// coreth `plugin/evm/upgrade/ap0/params.go:25` `MaximumExtraDataSize` — the
/// pre-ApricotPhase1 ceiling on `header.Extra` (`customheader/extra.go:158-166`,
/// `VerifyExtra`'s default arm).
const AP0_MAX_EXTRA_DATA_SIZE: usize = 64;

/// `customtypes.EmptyExtDataHash` = `keccak256(rlp(nil))` — the `ExtDataHash` of
/// a block with no atomic txs (coreth `hashes_ext.go`).
const EMPTY_EXT_DATA_HASH: [u8; 32] = [
    0x56, 0xe8, 0x1f, 0x17, 0x1b, 0xcc, 0x55, 0xa6, 0xff, 0x83, 0x45, 0xe6, 0x92, 0xc0, 0xf8, 0x6e,
    0x5b, 0x48, 0xe0, 0x1b, 0x99, 0x6c, 0xad, 0xc0, 0x01, 0x62, 0x2f, 0xb5, 0xe3, 0x63, 0xb4, 0x21,
];

/// The coreth C-Chain block header (`customtypes.HeaderSerializable`).
///
/// The 15 standard Ethereum header fields, then `ext_data_hash` (always
/// present), then the fork-gated optional tail. `Option<…>` mirrors the Go
/// `rlp:"optional"` pointer fields exactly: `None` ⇔ the field was absent on the
/// wire (and must stay absent on re-encode for byte parity). Big-int fields
/// (`difficulty`, `base_fee`, `ext_data_gas_used`, `block_gas_cost`) are
/// [`U256`] encoded as RLP scalars (minimal big-endian), matching Go
/// `WriteBigInt`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AvaHeader {
    /// `ParentHash`.
    pub parent_hash: B256,
    /// `UncleHash` (ommers hash).
    pub uncle_hash: B256,
    /// `Coinbase` (beneficiary, 20 bytes).
    pub coinbase: Address,
    /// `Root` (state root).
    pub state_root: B256,
    /// `TxHash` (transactions root).
    pub tx_root: B256,
    /// `ReceiptHash` (receipts root).
    pub receipt_root: B256,
    /// `Bloom` (256-byte logs bloom).
    pub bloom: Bytes,
    /// `Difficulty` (RLP scalar).
    pub difficulty: U256,
    /// `Number` (block height).
    pub number: u64,
    /// `GasLimit`.
    pub gas_limit: u64,
    /// `GasUsed`.
    pub gas_used: u64,
    /// `Time` (unix seconds).
    pub time: u64,
    /// `Extra` (extra data, arbitrary bytes).
    pub extra: Bytes,
    /// `MixDigest`.
    pub mix_digest: B256,
    /// `Nonce` (8-byte block nonce).
    pub nonce: [u8; 8],
    /// `ExtDataHash` — always present (field 16); `keccak256(rlp(ext_data))`.
    pub ext_data_hash: B256,
    /// `BaseFee` (AP3+, EIP-1559). RLP scalar.
    pub base_fee: Option<U256>,
    /// `ExtDataGasUsed` (AP4+). RLP scalar.
    pub ext_data_gas_used: Option<U256>,
    /// `BlockGasCost` (AP4+). RLP scalar.
    pub block_gas_cost: Option<U256>,
    /// `BlobGasUsed` (EIP-4844). RLP `uint64` (absent ⇒ encoded as `0x80`).
    pub blob_gas_used: Option<u64>,
    /// `ExcessBlobGas` (EIP-4844). RLP `uint64`.
    pub excess_blob_gas: Option<u64>,
    /// `ParentBeaconRoot` (EIP-4788).
    pub parent_beacon_root: Option<B256>,
    /// `TimeMilliseconds` (Granite). RLP `uint64`.
    pub time_milliseconds: Option<u64>,
    /// `MinDelayExcess` (Granite, ACP-226). RLP `uint64`.
    pub min_delay_excess: Option<u64>,
}

impl AvaHeader {
    /// Encodes the header as coreth does (`HeaderSerializable.EncodeRLP`):
    /// the standard fields + `ext_data_hash`, then the optional tail using the
    /// "any later present ⇒ all earlier present" rule. Byte-identical to Go.
    pub(crate) fn encode_rlp(&self, out: &mut Vec<u8>) {
        // Decide which optional fields are present (Go `_tmp1.._tmp8`).
        let t1 = self.base_fee.is_some();
        let t2 = self.ext_data_gas_used.is_some();
        let t3 = self.block_gas_cost.is_some();
        let t4 = self.blob_gas_used.is_some();
        let t5 = self.excess_blob_gas.is_some();
        let t6 = self.parent_beacon_root.is_some();
        let t7 = self.time_milliseconds.is_some();
        let t8 = self.min_delay_excess.is_some();

        // Build the payload first to learn its length for the list header.
        let mut payload = Vec::new();
        self.parent_hash.encode(&mut payload);
        self.uncle_hash.encode(&mut payload);
        self.coinbase.encode(&mut payload);
        self.state_root.encode(&mut payload);
        self.tx_root.encode(&mut payload);
        self.receipt_root.encode(&mut payload);
        self.bloom.encode(&mut payload);
        self.difficulty.encode(&mut payload);
        self.number.encode(&mut payload);
        self.gas_limit.encode(&mut payload);
        self.gas_used.encode(&mut payload);
        self.time.encode(&mut payload);
        self.extra.encode(&mut payload);
        self.mix_digest.encode(&mut payload);
        self.nonce.encode(&mut payload);
        self.ext_data_hash.encode(&mut payload);

        if t1 || t2 || t3 || t4 || t5 || t6 || t7 || t8 {
            encode_scalar_opt(self.base_fee, &mut payload);
        }
        if t2 || t3 || t4 || t5 || t6 || t7 || t8 {
            encode_scalar_opt(self.ext_data_gas_used, &mut payload);
        }
        if t3 || t4 || t5 || t6 || t7 || t8 {
            encode_scalar_opt(self.block_gas_cost, &mut payload);
        }
        if t4 || t5 || t6 || t7 || t8 {
            encode_u64_opt(self.blob_gas_used, &mut payload);
        }
        if t5 || t6 || t7 || t8 {
            encode_u64_opt(self.excess_blob_gas, &mut payload);
        }
        if t6 || t7 || t8 {
            match self.parent_beacon_root {
                Some(h) => h.encode(&mut payload),
                None => payload.push(RLP_EMPTY_STRING_CODE),
            }
        }
        if t7 || t8 {
            encode_u64_opt(self.time_milliseconds, &mut payload);
        }
        if t8 {
            encode_u64_opt(self.min_delay_excess, &mut payload);
        }

        RlpListHeader {
            list: true,
            payload_length: payload.len(),
        }
        .encode(out);
        out.extend_from_slice(&payload);
    }

    /// Decodes a coreth header from `buf` (advancing it past the header).
    fn decode_rlp(buf: &mut &[u8]) -> Result<Self> {
        let header = RlpListHeader::decode(buf).map_err(rlp_err)?;
        if !header.list {
            return Err(rlp_err(RlpError::UnexpectedString));
        }
        let payload_len = header.payload_length;
        if payload_len > buf.len() {
            return Err(rlp_err(RlpError::InputTooShort));
        }
        // Carve out exactly the header payload so trailing block fields are not
        // consumed; `body` is the cursor we decode the fields from.
        let (body_bytes, rest) = buf.split_at(payload_len);
        let mut body = body_bytes;
        let body = &mut body;

        let parent_hash = B256::decode(body).map_err(rlp_err)?;
        let uncle_hash = B256::decode(body).map_err(rlp_err)?;
        let coinbase = Address::decode(body).map_err(rlp_err)?;
        let state_root = B256::decode(body).map_err(rlp_err)?;
        let tx_root = B256::decode(body).map_err(rlp_err)?;
        let receipt_root = B256::decode(body).map_err(rlp_err)?;
        let bloom = Bytes::decode(body).map_err(rlp_err)?;
        let difficulty = U256::decode(body).map_err(rlp_err)?;
        let number = u64::decode(body).map_err(rlp_err)?;
        let gas_limit = u64::decode(body).map_err(rlp_err)?;
        let gas_used = u64::decode(body).map_err(rlp_err)?;
        let time = u64::decode(body).map_err(rlp_err)?;
        let extra = Bytes::decode(body).map_err(rlp_err)?;
        let mix_digest = B256::decode(body).map_err(rlp_err)?;
        let nonce = <[u8; 8]>::decode(body).map_err(rlp_err)?;
        let ext_data_hash = B256::decode(body).map_err(rlp_err)?;

        // Optional tail: decode while bytes remain, in order.
        let base_fee = decode_scalar_opt(body)?;
        let ext_data_gas_used = decode_scalar_opt(body)?;
        let block_gas_cost = decode_scalar_opt(body)?;
        let blob_gas_used = decode_u64_opt(body)?;
        let excess_blob_gas = decode_u64_opt(body)?;
        let parent_beacon_root = decode_b256_opt(body)?;
        let time_milliseconds = decode_u64_opt(body)?;
        let min_delay_excess = decode_u64_opt(body)?;

        if !body.is_empty() {
            return Err(rlp_err(RlpError::UnexpectedLength));
        }
        *buf = rest;

        Ok(Self {
            parent_hash,
            uncle_hash,
            coinbase,
            state_root,
            tx_root,
            receipt_root,
            bloom,
            difficulty,
            number,
            gas_limit,
            gas_used,
            time,
            extra,
            mix_digest,
            nonce,
            ext_data_hash,
            base_fee,
            ext_data_gas_used,
            block_gas_cost,
            blob_gas_used,
            excess_blob_gas,
            parent_beacon_root,
            time_milliseconds,
            min_delay_excess,
        })
    }

    /// The block ID / hash = `keccak256(header RLP)` (coreth `RLPHash(header)`).
    #[must_use]
    pub fn hash(&self) -> B256 {
        let mut bytes = Vec::new();
        self.encode_rlp(&mut bytes);
        keccak256(&bytes)
    }
}

/// A decoded / about-to-be-assembled C-Chain block, decoupled from the verify
/// lifecycle ([`EvmBlock`]). Carries the EVM body (header, txs) plus the
/// Avalanche additions (`version`, `ext_data`, the extracted `atomic_txs`).
#[derive(Clone, Debug)]
pub struct AvaBlockParts {
    /// The coreth header (carries the optional tail + `ext_data_hash`).
    pub header: AvaHeader,
    /// The signed EVM transactions (block body `Txs`).
    pub transactions: Vec<TransactionSigned>,
    /// The atomic Import/Export txs extracted from `ext_data` (§6.2).
    pub atomic_txs: Vec<AtomicTx>,
    /// The raw `ExtData` bytes (the pre-image of `ext_data_hash`).
    pub ext_data: Vec<u8>,
    /// The block `Version` (coreth `BlockBodyExtra.Version`).
    pub version: u32,
}

/// A C-Chain block in one of the spec-06 lifecycle states (§3.1). Wire decode
/// yields [`EvmBlock::Unverified`]; the on-chain builder yields
/// [`EvmBlock::Built`]. Both wrap the same decoded [`AvaBlockParts`] plus the
/// canonical encoded bytes (so `encoded_bytes()` is the exact coreth wire form)
/// and the cached block hash.
#[derive(Clone, Debug)]
pub enum EvmBlock {
    /// A block parsed from wire bytes (`ChainVm::parse_block`), not yet verified.
    Unverified(EvmBlockInner),
    /// A block produced locally by the builder (§4), ready to be proposed.
    Built(EvmBlockInner),
}

/// The shared payload of an [`EvmBlock`] in any state.
#[derive(Clone, Debug)]
pub struct EvmBlockInner {
    parts: AvaBlockParts,
    /// The canonical coreth wire encoding (`Block::bytes`).
    encoded: Vec<u8>,
    /// `keccak256(header RLP)` — the block ID, cached.
    hash: B256,
}

impl EvmBlock {
    fn inner(&self) -> &EvmBlockInner {
        match self {
            EvmBlock::Unverified(i) | EvmBlock::Built(i) => i,
        }
    }

    /// The block ID = `keccak256(header RLP)` (consensus-critical, §9.3).
    #[must_use]
    pub fn hash(&self) -> B256 {
        self.inner().hash
    }

    /// The block height.
    #[must_use]
    pub fn number(&self) -> u64 {
        self.inner().parts.header.number
    }

    /// The coreth header.
    #[must_use]
    pub fn header(&self) -> &AvaHeader {
        &self.inner().parts.header
    }

    /// The header's declared state root (`header.Root`) — the value `verify`
    /// asserts the Firewood pre-commit root equals (spec 10 §3.2).
    #[must_use]
    pub fn header_state_root(&self) -> &B256 {
        &self.inner().parts.header.state_root
    }

    /// The header's parent hash (`header.ParentHash`).
    #[must_use]
    pub fn parent_hash(&self) -> &B256 {
        &self.inner().parts.header.parent_hash
    }

    /// The decoded block parts (header, txs, atomic txs, ext data, version).
    #[must_use]
    pub fn parts(&self) -> &AvaBlockParts {
        &self.inner().parts
    }

    /// Consumes the block, returning its parts (header + body). Used by the
    /// builder / re-assembly paths and tests that need to adjust a field and
    /// re-assemble (e.g. patch `header.state_root` to the executor's root).
    #[must_use]
    pub fn into_parts(self) -> AvaBlockParts {
        match self {
            EvmBlock::Unverified(i) | EvmBlock::Built(i) => i.parts,
        }
    }

    /// The signed EVM transactions (block body).
    #[must_use]
    pub fn transactions(&self) -> &[TransactionSigned] {
        &self.inner().parts.transactions
    }

    /// The atomic Import/Export txs extracted from `ExtData` (§6.2).
    #[must_use]
    pub fn atomic_txs(&self) -> &[AtomicTx] {
        &self.inner().parts.atomic_txs
    }

    /// The raw `ExtData` bytes (pre-image of `ext_data_hash`).
    #[must_use]
    pub fn ext_data(&self) -> &[u8] {
        &self.inner().parts.ext_data
    }

    /// The block `Version`.
    #[must_use]
    pub fn version(&self) -> u32 {
        self.inner().parts.version
    }

    /// The canonical coreth wire bytes (`Block::bytes`).
    #[must_use]
    pub fn encoded_bytes(&self) -> &[u8] {
        &self.inner().encoded
    }

    /// Recovers the sender of every EVM transaction (spec 10 §9.3). The atomic
    /// txs are *not* EVM txs and carry their own fx credentials, so they are not
    /// recovered here.
    ///
    /// # Errors
    /// Returns [`Error::NilTx`] if a signature fails to recover.
    pub fn recover_senders(&self) -> Result<Vec<RecoveredTx>> {
        self.transactions()
            .iter()
            .map(|tx| tx.clone().try_into_recovered().map_err(|_| Error::NilTx))
            .collect()
    }

    /// Builds the reth [`Header`] this block executes as (the env header for
    /// `evm_env_for_header`). Maps the consensus-relevant fields of the coreth
    /// [`AvaHeader`] (parent hash, number, timestamp, gas limit, base fee,
    /// coinbase, extra data) onto reth's standard header; the coreth-specific
    /// extras (`ext_data_hash`, AP4 fields, …) are not part of the EVM execution
    /// env. The base fee is narrowed to `u64` (a header base fee never exceeds
    /// `u64::MAX` wei on the C-Chain; an out-of-range value is a malformed header).
    fn eth_env_header(&self) -> Result<Header> {
        let h = self.header();
        let base_fee_per_gas = match h.base_fee {
            Some(bf) => Some(u64::try_from(bf).map_err(|_| Error::NilBaseFee)?),
            None => None,
        };
        Ok(Header {
            parent_hash: h.parent_hash,
            ommers_hash: h.uncle_hash,
            beneficiary: h.coinbase,
            state_root: h.state_root,
            transactions_root: h.tx_root,
            receipts_root: h.receipt_root,
            difficulty: h.difficulty,
            number: h.number,
            gas_limit: h.gas_limit,
            gas_used: h.gas_used,
            timestamp: h.time,
            extra_data: h.extra.clone(),
            mix_hash: h.mix_digest,
            base_fee_per_gas,
            // The Cancun tail (EIP-4844 blob fields + the EIP-4788 parent beacon
            // root) MUST be carried through to the execution env: alloy-evm's
            // beacon-root system call errors with `MissingParentBeaconBlockRoot`
            // for a Cancun-active block whose env header lacks the root (coreth
            // activates Cancun with Etna, so every local-network block ≥ 1
            // carries `parentBeaconRoot = 0x0`; coreth runs
            // `ProcessBeaconBlockRoot` on it — `core/state_processor.go`).
            // Dropping these fields made the follower reject every live Go
            // block with "EIP-4788 parent beacon block root missing" (M9.15
            // rung 5).
            blob_gas_used: h.blob_gas_used,
            excess_blob_gas: h.excess_blob_gas,
            parent_beacon_block_root: h.parent_beacon_root,
            ..Default::default()
        })
    }
}

/// The dependencies the [`EvmBlock`] lifecycle (§3.1) needs: the Firewood
/// state-of-record provider, the EVM config (executor), and the canonical
/// block-metadata store (G6). Held by the `ChainVm` adapter (M6.10) and passed by
/// reference into `verify`/`accept`/`reject`.
///
/// The synchronous spec-06 `Block` trait (`ava_snow::snowman::Block`) is `&self`
/// with no VM handle, so the lifecycle is exposed here as inherent methods that
/// take this context explicitly; the trait `impl` (which bundles a block with its
/// context) lands with the adapter in M6.10.
pub struct EvmBlockContext {
    state: Arc<FirewoodStateProvider>,
    evm_config: AvaEvmConfig,
    canonical: Arc<CanonicalStore>,
    /// The atomic backend (atomic trie + shared-memory apply), wired in via
    /// [`EvmBlockContext::with_atomic_backend`] (M6.17, §6.4/§17.4). `None` until
    /// configured — `accept` skips atomic indexing when absent (e.g. a chain with
    /// no cross-chain activity, or tests that exercise only EVM state).
    atomic_backend: Option<Arc<AtomicBackend>>,
    /// The warp accept seam (M6.31, spec 20 §3.1): the registered
    /// [`WarpPrecompile`] instance (its `SendWarpMessage` log buffer) + the
    /// [`WarpBackend`] that records accepted unsigned messages for signing.
    /// `None` until wired via [`EvmBlockContext::with_warp`].
    warp: Option<WarpAcceptSeam>,
    /// The verify-time receipt stash (cchain-tx-pipeline task 3): `verify`'s
    /// `execute_batch` outcome carries the ONLY copy of this block's receipts,
    /// so it stashes them here keyed by pre-commit root (the same warp-seam
    /// idiom as `warp`'s `pending` map, always present — unlike `warp`/
    /// `atomic_backend` this is not an optional feature). `accept` takes +
    /// persists the entry; `reject` drops it.
    receipts: parking_lot::Mutex<std::collections::BTreeMap<B256, Vec<EthReceipt>>>,
    /// The accepted-tx index [`EvmBlock::accept`] records each block's
    /// [`TxReceiptRecord`]s into (cchain-tx-pipeline task 3, Task 4's RPC
    /// reader). Defaults to a fresh, unshared index; [`EvmVm`](crate::vm::EvmVm)
    /// overrides it via [`EvmBlockContext::with_accepted_tx_index`] so the RPC
    /// handlers (`EvmVm::accepted_tx_index`) observe the SAME instance the
    /// block lifecycle writes into.
    accepted_tx_index: Arc<AcceptedTxIndex>,
}

/// The accept-time warp routing seam (M6.31, coreth `handlePrecompileAccept`):
/// `verify` drains [`WarpPrecompile::take_logs`] and stashes them keyed by the
/// pre-commit root; `accept` routes the accepted root's logs into the
/// [`WarpBackend`] (BEFORE the canonical append, matching coreth's
/// before-chain-Accept ordering); `reject` discards them.
struct WarpAcceptSeam {
    /// The SAME [`WarpPrecompile`] instance registered in the precompile
    /// registry (its internal log buffer accumulates this block's sends).
    precompile: Arc<WarpPrecompile>,
    /// Records accepted unsigned messages so the node will sign them.
    backend: Arc<WarpBackend>,
    /// Per-verified-block `SendWarpMessage` logs keyed by pre-commit root.
    /// Verifies are serial under consensus (one block verified at a time), so
    /// the drained buffer is attributable to the just-verified block.
    pending: parking_lot::Mutex<std::collections::BTreeMap<B256, Vec<WarpLog>>>,
}

impl EvmBlockContext {
    /// Builds a lifecycle context from its three collaborators (no atomic
    /// backend; see [`EvmBlockContext::with_atomic_backend`] to add one).
    #[must_use]
    pub fn new(
        state: Arc<FirewoodStateProvider>,
        evm_config: AvaEvmConfig,
        canonical: Arc<CanonicalStore>,
    ) -> Self {
        Self {
            state,
            evm_config,
            canonical,
            atomic_backend: None,
            warp: None,
            receipts: parking_lot::Mutex::new(std::collections::BTreeMap::new()),
            accepted_tx_index: Arc::new(AcceptedTxIndex::new()),
        }
    }

    /// Attaches the warp accept seam (M6.31, spec 20 §3.1): `precompile` MUST
    /// be the same instance registered in the EVM config's
    /// [`crate::precompile::registry::PrecompileRegistry`] (its log buffer is
    /// what `verify` drains); `backend` receives the accepted blocks' unsigned
    /// messages. Additive — existing callers keep the no-warp behavior.
    #[must_use]
    pub fn with_warp(mut self, precompile: Arc<WarpPrecompile>, backend: Arc<WarpBackend>) -> Self {
        self.warp = Some(WarpAcceptSeam {
            precompile,
            backend,
            pending: parking_lot::Mutex::new(std::collections::BTreeMap::new()),
        });
        self
    }

    /// Attaches an [`AtomicBackend`] so [`EvmBlock::accept`] indexes the block's
    /// atomic txs into the atomic trie and applies the cross-chain shared-memory
    /// batch (§17.4). Additive — existing callers keep the no-atomic behavior.
    #[must_use]
    pub fn with_atomic_backend(mut self, atomic_backend: Arc<AtomicBackend>) -> Self {
        self.atomic_backend = Some(atomic_backend);
        self
    }

    /// The attached atomic backend, if any.
    #[must_use]
    pub fn atomic_backend(&self) -> Option<&Arc<AtomicBackend>> {
        self.atomic_backend.as_ref()
    }

    /// Overrides the accepted-tx index [`EvmBlock::accept`] records into
    /// (cchain-tx-pipeline task 3). [`EvmVm`](crate::vm::EvmVm) calls this with
    /// its own shared instance so the `avax.*`/`eth_*` RPC handlers observe the
    /// same accepted receipts the lifecycle writes; callers that never attach
    /// one (most tests) keep the fresh per-context default from
    /// [`EvmBlockContext::new`].
    #[must_use]
    pub fn with_accepted_tx_index(mut self, accepted_tx_index: Arc<AcceptedTxIndex>) -> Self {
        self.accepted_tx_index = accepted_tx_index;
        self
    }

    /// The accepted-tx index this context's `accept` writes into.
    #[must_use]
    pub fn accepted_tx_index(&self) -> &Arc<AcceptedTxIndex> {
        &self.accepted_tx_index
    }

    /// Test-only seam (cchain-tx-pipeline task 3, I1 review fix): overwrites
    /// the verify-time receipt stash for `root`, bypassing a real `verify`.
    /// Lets a test exercise `accept`'s never-fail posture against a
    /// corrupted/mismatched stash (e.g. a receipts/tx-count disagreement)
    /// without fabricating one through full semantic execution. NOT part of
    /// the lifecycle contract — production code only ever reaches this stash
    /// through [`EvmBlock::verify`].
    #[doc(hidden)]
    pub fn stash_receipts_for_test(&self, root: B256, receipts: Vec<EthReceipt>) {
        self.receipts.lock().insert(root, receipts);
    }

    /// The Firewood state-of-record provider.
    #[must_use]
    pub fn state(&self) -> &Arc<FirewoodStateProvider> {
        &self.state
    }

    /// The canonical (non-state) block-metadata store (G6).
    #[must_use]
    pub fn canonical(&self) -> &Arc<CanonicalStore> {
        &self.canonical
    }

    /// The chain spec backing the EVM config (used to decode block bytes).
    #[must_use]
    pub fn chain_spec(&self) -> &AvaChainSpec {
        self.evm_config.chain_spec().as_ref()
    }

    /// The EVM config (executor) backing this context.
    #[must_use]
    pub fn evm_config(&self) -> &AvaEvmConfig {
        &self.evm_config
    }

    /// The union of UTXO ids consumed by atomic txs in the still-**processing**
    /// ancestry of the block under verification, back to the last-accepted block
    /// (coreth `verifyTxs` → `conflicts`). The conflict-set verify
    /// ([`crate::atomic::verify::verify_no_conflicts`]) rejects a block whose
    /// atomic inputs overlap this set.
    ///
    /// On the linear-accept path (the parent IS last-accepted) the processing
    /// ancestry is empty, so this is empty — coreth's `conflicts` returns
    /// immediately once it walks past last-accepted, and an accepted parent's
    /// UTXOs are already proven removed by the per-tx shared-memory `Get`. The
    /// non-linear (sibling/processing-fork) ancestry is threaded in by the
    /// `ChainVm` adapter (M6.10) once it owns the verified-block tree; until then
    /// the intra-block conflict check (always applied) is the operative guard.
    #[must_use]
    pub fn processing_ancestor_inputs(&self) -> std::collections::BTreeSet<ava_types::id::Id> {
        std::collections::BTreeSet::new()
    }
}

impl EvmBlock {
    /// **Verify** (spec 10 §3.1/§3.2, 06 linear acceptance): semantic-execute this
    /// block against its parent state and compute the Firewood **pre-commit root**
    /// without committing.
    ///
    /// Steps:
    /// 1. Recover EVM tx senders.
    /// 2. Open a Firewood read view at `parent_state_root` and an in-memory revm
    ///    overlay over it.
    /// 3. Drive [`AvaEvmConfig::execute_batch`] (the bare reth `BlockExecutor`)
    ///    over the recovered txs.
    /// 4. Convert the returned `BundleState` to a Firewood proposal via
    ///    [`FirewoodStateProvider::propose_from_bundle`] — this computes the
    ///    pre-commit root **and stashes** the proposal ops keyed by that root, so
    ///    [`EvmBlock::accept`] can commit exactly it.
    /// 5. Assert the pre-commit root equals `header.state_root` and that the
    ///    executed gas matches `header.gas_used`.
    ///
    /// Returns the verified pre-commit root (== `header.state_root`). The EVM tip
    /// is **not** advanced (the proposal is only committed on accept).
    ///
    /// # Errors
    /// Returns [`Error`] if the parent view is unavailable, execution fails, the
    /// computed root disagrees with the header, or gas usage disagrees.
    pub fn verify(&self, ctx: &EvmBlockContext, parent_state_root: B256) -> Result<B256> {
        self.verify_with_predicates(ctx, parent_state_root, &AvaExecCtx::default())
    }

    /// [`EvmBlock::verify`] with an explicit per-block precompile execution
    /// context (M6.31, spec 10 §6.5/§17.5): the verified warp predicate results
    /// (from the pre-execution predicate pass,
    /// [`crate::precompile::warp::build_block_predicates`], keyed by tx index)
    /// and the proposervm-pinned P-Chain height. The `ChainVm` adapter runs the
    /// async predicate pass against its `ValidatorState` first, then verifies
    /// with the results threaded in; [`EvmBlock::verify`] is the
    /// no-warp-predicates path.
    ///
    /// # Errors
    /// As [`EvmBlock::verify`].
    /// Full `wrappedBlock.syntacticVerify` port (M9.15 task L1 + task 5 —
    /// every structural check, including `Difficulty == 1` and `VerifyExtra`).
    ///
    /// Mirrors coreth `wrappedBlock.syntacticVerify`
    /// (`plugin/evm/wrapped_block.go:398-527`) **in Go's check order**, so the
    /// first error a malformed block hits matches Go's rejection exactly:
    /// number → difficulty → nonce → mixDigest → VerifyExtra → version →
    /// txsHash → uncleHash → coinbase → min-gas-price → BaseFee →
    /// BlockGasCost → the Cancun header clamp + body blob-count parity
    /// (`core/block_validator.go:100-104`).
    fn syntactic_verify(&self, spec: &AvaChainSpec) -> Result<()> {
        let h = self.header();

        // coreth wrapped_block.go:412 — block number must fit uint64.
        // `AvaHeader::number` already decodes as a Rust `u64` (never a wider
        // integer, see `AvaHeader::decode_rlp`), so this can never fire; kept
        // to preserve Go's check order + sentinel for Task 6's
        // rejection-class mapping.
        if U256::from(h.number) > U256::from(u64::MAX) {
            return Err(Error::InvalidBlockNumber(U256::from(h.number)));
        }

        // coreth wrapped_block.go:415 — difficulty must be exactly 1 (the
        // dummy consensus engine's `Prepare` stamps every header this way,
        // `consensus/dummy/consensus.go:233-235`).
        if h.difficulty != U256::from(1) {
            return Err(Error::InvalidDifficulty(h.difficulty));
        }

        // coreth wrapped_block.go:418 — nonce must be 0.
        if h.nonce != [0u8; 8] {
            return Err(Error::InvalidNonce(u64::from_be_bytes(h.nonce)));
        }

        // Ungated header invariant (coreth `wrapped_block.go:420-421`, applied
        // to every non-genesis block): MixDigest must be the zero hash.
        // Closes an adversarial PREVRANDAO fail-open.
        if h.mix_digest != B256::ZERO {
            return Err(Error::InvalidMixDigest(h.mix_digest));
        }

        // The phase is needed here (VerifyExtra) and again below (min-gas-price
        // / BaseFee / BlockGasCost); resolved once and reused.
        let phase = spec.fork_at(h.time);

        // coreth `customheader/extra.go:115-168` (`VerifyExtra`) — fork-keyed
        // header.Extra length rules.
        //
        // upstream-delta: coreth's `IsHelicon` arm (extra.go:120-121) skips
        // this check entirely; `AvaPhase` has no Helicon variant yet (it maps
        // to `None`, see `chainspec.rs`'s `AvaPhase::from_version_fork`), so
        // this port cannot express that arm. Fold it in when `AvaPhase` grows
        // Helicon.
        let extra_len = h.extra.len();
        match phase {
            p if p >= AvaPhase::Fortuna => {
                if extra_len < acp176::STATE_SIZE {
                    return Err(Error::InvalidExtraLength {
                        expected: ">= 24",
                        got: extra_len,
                    });
                }
            }
            p if p >= AvaPhase::Durango => {
                if extra_len < window::WINDOW_SIZE {
                    return Err(Error::InvalidExtraLength {
                        expected: ">= 80",
                        got: extra_len,
                    });
                }
            }
            p if p >= AvaPhase::ApricotPhase3 => {
                if extra_len != window::WINDOW_SIZE {
                    return Err(Error::InvalidExtraLength {
                        expected: "== 80",
                        got: extra_len,
                    });
                }
            }
            p if p >= AvaPhase::ApricotPhase1 => {
                if extra_len != 0 {
                    return Err(Error::InvalidExtraLength {
                        expected: "== 0",
                        got: extra_len,
                    });
                }
            }
            _ => {
                if extra_len > AP0_MAX_EXTRA_DATA_SIZE {
                    return Err(Error::InvalidExtraLength {
                        expected: "<= 64",
                        got: extra_len,
                    });
                }
            }
        }

        // coreth wrapped_block.go:434 — body extension version must be 0.
        if self.version() != 0 {
            return Err(Error::InvalidBlockVersion(self.version()));
        }

        // coreth wrapped_block.go:439 — header txsHash matches the body.
        let calculated_tx_root = calculate_transaction_root(&self.parts().transactions);
        if calculated_tx_root != h.tx_root {
            return Err(Error::TxRootMismatch {
                header: h.tx_root,
                calculated: calculated_tx_root,
            });
        }

        // coreth wrapped_block.go:444/:453 — uncle hash matches the
        // (structurally empty) body; the RLP decode (`decode_uncle_list`)
        // admits only an empty uncle list, so `CalcUncleHash(uncles)` is
        // always the empty-ommer sentinel and `errUnclesUnsupported`
        // (:453-455) can never separately fire.
        if h.uncle_hash != EMPTY_OMMER_ROOT_HASH {
            return Err(Error::InvalidUncleHash(h.uncle_hash));
        }

        // coreth wrapped_block.go:449 — C-Chain coinbase is the blackhole
        // address.
        if h.coinbase != BLACKHOLE_ADDRESS {
            return Err(Error::InvalidCoinbase(h.coinbase));
        }

        // coreth wrapped_block.go:458-473 — pre-AP1/pre-AP3 minimum gas
        // prices, enforced before dynamic fees (AP3+) go into effect.
        // (`phase` was already resolved above, for VerifyExtra.)
        if phase < AvaPhase::ApricotPhase1 {
            self.check_min_gas_price(AP0_MIN_GAS_PRICE)?;
        } else if phase < AvaPhase::ApricotPhase3 {
            self.check_min_gas_price(AP1_MIN_GAS_PRICE)?;
        }

        // coreth wrapped_block.go:476-483 — BaseFee non-nil at AP3+ (the
        // `BitLen() <= 256` half is structurally guaranteed: `base_fee` is a
        // `U256`).
        if phase >= AvaPhase::ApricotPhase3 && h.base_fee.is_none() {
            return Err(Error::NilBaseFee);
        }

        // coreth wrapped_block.go:486-495 — BlockGasCost non-nil + uint64 at
        // AP4+ (header verification already checks `BlockGasCost`
        // correctness upstream in Go; here only nil-ness/range is enforced).
        if phase >= AvaPhase::ApricotPhase4 {
            match h.block_gas_cost {
                Some(v) if v <= U256::from(u64::MAX) => {}
                other => return Err(Error::InvalidBlockGasCost(other)),
            }
        }

        if phase >= AvaPhase::Etna {
            match h.parent_beacon_root {
                None => return Err(Error::MissingParentBeaconRoot),
                Some(root) if root != B256::ZERO => {
                    return Err(Error::ParentBeaconRootNonEmpty(root));
                }
                Some(_) => {}
            }
            match h.blob_gas_used {
                None => return Err(Error::BlobGasUsedNilInCancun),
                Some(0) => {}
                Some(used) => return Err(Error::BlobsNotEnabled(used)),
            }
            match h.excess_blob_gas {
                Some(0) => {}
                other => return Err(Error::InvalidExcessBlobGas(other)),
            }
        } else {
            if h.parent_beacon_root.is_some() {
                return Err(Error::ParentBeaconRootBeforeCancun);
            }
            if h.excess_blob_gas.is_some() {
                return Err(Error::ExcessBlobGasBeforeCancun);
            }
            if h.blob_gas_used.is_some() {
                return Err(Error::BlobGasUsedBeforeCancun);
            }
        }

        // Body blob-count parity (`ValidateBody`): the header's blobGasUsed
        // (clamped to 0 above at Cancun; absent == 0 pre-Cancun) must equal
        // the blob gas implied by the body's blob hashes.
        let blobs: u64 = self
            .parts()
            .transactions
            .iter()
            .map(|tx| {
                tx.blob_versioned_hashes()
                    .map_or(0, |hashes| hashes.len() as u64)
            })
            .sum();
        let calculated = blobs.saturating_mul(ava_evm_reth::DATA_GAS_PER_BLOB);
        let declared = h.blob_gas_used.unwrap_or(0);
        if declared != calculated {
            return Err(Error::BlobGasUsedMismatch {
                header: declared,
                calculated,
            });
        }
        Ok(())
    }

    /// coreth `wrapped_block.go:458-473` — rejects the block if any tx's
    /// `GasPrice()` (the fee cap, for dynamic-fee txs) is below `min`.
    fn check_min_gas_price(&self, min: u128) -> Result<()> {
        for tx in &self.parts().transactions {
            let have = tx.max_fee_per_gas();
            if have < min {
                return Err(Error::GasPriceTooLow {
                    tx: *tx.hash(),
                    have,
                    min,
                });
            }
        }
        Ok(())
    }

    pub fn verify_with_predicates(
        &self,
        ctx: &EvmBlockContext,
        parent_state_root: B256,
        exec_ctx: &AvaExecCtx,
    ) -> Result<B256> {
        // Structural syntacticVerify port — runs before any execution work
        // (coreth fail-closes these in `wrappedBlock.syntacticVerify` before
        // the state transition; a violating block is invalid regardless of its
        // declared state root).
        self.syntactic_verify(ctx.chain_spec())?;

        // Atomic semantic verify (spec 10 §6.5, coreth `verifyTxs`): reject the
        // block if its atomic txs double-spend each other (intra-block conflict)
        // or a still-processing ancestor's atomic inputs. Linear-accept is the
        // common case (parent == last-accepted), where the processing ancestry is
        // empty and only the intra-block conflict can fire; the processing-ancestry
        // input union is threaded in by the `ChainVm` adapter (M6.10) once it has
        // the verified-block tree. Runs before EVM execution — a conflicting block
        // is invalid regardless of its EVM state transition (cheaper to reject).
        let unsigned: Vec<_> = self
            .atomic_txs()
            .iter()
            .map(|tx| tx.unsigned.clone())
            .collect();
        crate::atomic::verify::verify_no_conflicts(&unsigned, &ctx.processing_ancestor_inputs())?;

        let txs = self.recover_senders()?;

        // Parent state view + revm overlay (the verify path, spec 10 §3.2).
        let view = ctx.state.history_by_state_root(parent_state_root)?;
        let mut state: State<StateProviderDatabase<FirewoodStateView>> = StateBuilder::new()
            .with_database(StateProviderDatabase::new(view))
            .with_bundle_update()
            .build();

        // Semantic execute the EVM txs (atomic pre-hook is NoopPreHook here; the
        // atomic Import/Export pre-hook is wired with the atomic backend, M6.15).
        let env = ctx.evm_config.evm_env_for_header(&self.eth_env_header()?);
        let outcome =
            ctx.evm_config
                .execute_batch_with_ctx(env, &mut state, &NoopPreHook, &txs, exec_ctx)?;

        // Drain this verify's `SendWarpMessage` logs from the registered warp
        // precompile NOW (whether or not the block proves valid below) so a
        // failed verify cannot leak its logs into the next block's accept
        // (M6.31, spec 20 §3.1). They are stashed keyed by pre-commit root only
        // once the block verifies.
        let warp_logs = ctx
            .warp
            .as_ref()
            .map(|seam| seam.precompile.take_logs())
            .unwrap_or_default();

        // Pre-commit root via Firewood propose (NOT committed); stashes by root.
        let precommit = ctx.state.propose_from_bundle(&outcome.bundle)?;

        // The load-bearing semantic check (spec 10 §3.2): the computed pre-commit
        // root must equal the header's declared state root.
        let declared = self.header().state_root;
        if precommit != declared {
            // Drop the just-stashed proposal: this block is invalid, nothing should
            // remain commit-able for this root.
            ctx.state.discard(precommit);
            return Err(Error::MissingProposal(declared));
        }
        // Gas accounting must agree with the header.
        if outcome.result.gas_used != self.header().gas_used {
            ctx.state.discard(precommit);
            return Err(Error::NoGasUsed);
        }

        // Stash the verified block's warp logs for accept-time routing.
        if let Some(seam) = ctx.warp.as_ref() {
            seam.pending.lock().insert(precommit, warp_logs);
        }

        // Stash this verified block's per-tx receipts for accept-time persist +
        // index (cchain-tx-pipeline task 3): `outcome.result.receipts` is the
        // ONLY place these exist — `accept` has no way to re-derive them
        // without re-executing. Moved (not cloned): `outcome` is not read again
        // after this point. `reject` removes this entry (below) so a failed
        // verify never leaks receipts into a later accept, mirroring the warp
        // seam immediately above.
        ctx.receipts
            .lock()
            .insert(precommit, outcome.result.receipts);

        Ok(precommit)
    }

    /// **Accept** (spec 10 §3.1, 06 `accept_preferred_child`): linear accept —
    /// the parent IS `last_accepted`. Commits the stashed Firewood proposal for
    /// `precommit_root` (durably advancing the EVM tip), then appends this block to
    /// the canonical store and advances the tip pointer. No reorgs.
    ///
    /// `precommit_root` is the value [`EvmBlock::verify`] returned for this block.
    ///
    /// # Errors
    /// Returns [`Error::MissingProposal`] if no proposal is stashed for
    /// `precommit_root` (verify was not run, or it was rejected), or a store error.
    pub fn accept(&self, ctx: &EvmBlockContext, precommit_root: B256) -> Result<()> {
        // 0. Warp precompile accept (M6.31, coreth `handlePrecompileAccept`,
        //    spec 20 §3.1): route this block's `SendWarpMessage` logs (stashed
        //    at verify time keyed by the pre-commit root) into the
        //    `WarpBackend` so the node will sign them. BEFORE the state commit
        //    + canonical append, matching coreth's before-chain-Accept order.
        if let Some(seam) = ctx.warp.as_ref() {
            let logs = seam.pending.lock().remove(&precommit_root);
            if let Some(logs) = logs {
                handle_precompile_accept(&seam.backend, &logs)?;
            }
        }

        // 1. Commit the Firewood proposal -> durably advances the EVM state tip.
        ctx.state.commit(precommit_root)?;

        // 2. AtomicBackend indexing (§6.4/§17.4): AFTER the EVM state commit,
        //    index this block's atomic txs into the atomic trie and apply the
        //    cross-chain shared-memory batch (Import → Remove on source, Export →
        //    Put on dest). Skipped when no backend is attached (M6.17).
        if let Some(backend) = ctx.atomic_backend.as_ref() {
            backend.accept(self.number(), self.atomic_txs())?;
        }

        // 2.5 Receipts (cchain-tx-pipeline task 3): take the verify-time stash
        //     for this pre-commit root. Its absence means `verify` never ran in
        //     THIS process (e.g. an accept-only replay/resume path) — that is
        //     NEVER an accept failure: persist an empty receipts list and skip
        //     indexing below, exactly the M6.24 placeholder behavior this task
        //     replaces for the common (verify-then-accept) case.
        let receipts = ctx.receipts.lock().remove(&precommit_root).unwrap_or_else(|| {
            tracing::debug!(
                block_hash = %self.hash(),
                block_number = self.number(),
                precommit_root = %precommit_root,
                "no verify-time receipt stash for this pre-commit root; persisting empty receipts"
            );
            Vec::new()
        });
        let encoded_receipts = encode_block_receipts(&receipts);

        // 3. Append non-state block metadata + advance the canonical tip (G6,
        //    §17.7). precompile-accept callbacks (§8) are wired by M6.22.
        ctx.canonical.append_canonical(
            self.number(),
            self.hash(),
            self.header().state_root,
            self.ext_data(),
            &encoded_receipts,
        )?;

        // 4. `tx_hash -> block number` rows + the `AcceptedTxIndex` (task 3;
        //    Task 4's RPC layer is the reader). This block is ALREADY durably
        //    accepted by this point (steps 1-3 succeeded) — an indexing
        //    failure here must NEVER fail `accept` (I1 review fix): log at
        //    `warn!` and continue. A no-op when the stash was missing above
        //    (`receipts` is empty then).
        if !receipts.is_empty()
            && let Err(e) = self.index_accepted_receipts(ctx, &receipts)
        {
            tracing::warn!(
                block_hash = %self.hash(),
                block_number = self.number(),
                error = %e,
                "failed to index accepted tx receipts after the block was \
                 already durably accepted (tx_number rows / AcceptedTxIndex \
                 may be incomplete for this block); continuing"
            );
        }
        Ok(())
    }

    /// Builds + records a [`TxReceiptRecord`] per tx and writes the
    /// `tx_hash -> block number` row (cchain-tx-pipeline task 3). `receipts`
    /// MUST be this block's receipts in the same order as
    /// [`EvmBlock::transactions`] (the order `execute_batch` produced them in
    /// at verify time — the invariant [`EvmBlock::accept`] relies on to zip
    /// them against the re-recovered senders here); a length mismatch is
    /// treated as a corrupted stash, not a panic.
    ///
    /// Called ONLY after [`EvmBlock::accept`] has already durably committed
    /// the block (state committed + canonical appended) — every error path
    /// here is caught and logged by the caller, NEVER propagated to fail
    /// `accept` (I1 review fix). Each tx's `tx_number` row and
    /// `AcceptedTxIndex` entry are written together in the same loop
    /// iteration, so a failure partway through (e.g. a KV write error) leaves
    /// the two indices consistent with each other for every tx processed so
    /// far — never a `tx_number` row with no matching `AcceptedTxIndex` entry.
    ///
    /// # Errors
    /// Returns [`Error::NilTx`] if a signature fails to recover (never
    /// expected — the same txs recovered cleanly at verify time),
    /// [`Error::ReceiptTxCountMismatch`] if `receipts.len()` disagrees with
    /// this block's tx count, [`Error::FeeOverflow`] if a receipt's
    /// cumulative gas used is not monotonically non-decreasing (a corrupted
    /// receipt list), or a canonical-store KV write error.
    fn index_accepted_receipts(
        &self,
        ctx: &EvmBlockContext,
        receipts: &[EthReceipt],
    ) -> Result<()> {
        let txs = self.recover_senders()?;
        if txs.len() != receipts.len() {
            return Err(Error::ReceiptTxCountMismatch {
                txs: txs.len(),
                receipts: receipts.len(),
            });
        }
        let base_fee = self.eth_env_header()?.base_fee_per_gas;
        let block_hash = self.hash();
        let block_number = self.number();

        let mut cumulative_before = 0u64;
        // Block-wide running log counter (go-ethereum `core/types.Receipts.
        // DeriveFields`: each log's `Index` is a running count across the
        // WHOLE block, not reset per tx). `first_log_index` for tx N is this
        // counter's value BEFORE tx N's own logs are added.
        let mut log_count_before = 0u64;
        for (idx, (tx, receipt)) in txs.iter().zip(receipts.iter()).enumerate() {
            let tx_hash = *tx.tx_hash();

            let gas_used = receipt
                .cumulative_gas_used
                .checked_sub(cumulative_before)
                .ok_or(Error::FeeOverflow)?;
            cumulative_before = receipt.cumulative_gas_used;

            let first_log_index = log_count_before;
            let this_tx_log_count =
                u64::try_from(receipt.logs.len()).map_err(|_| Error::FeeOverflow)?;
            log_count_before = log_count_before
                .checked_add(this_tx_log_count)
                .ok_or(Error::FeeOverflow)?;

            // coreth/geth `types.Receipt.ContractAddress`: the CREATE address
            // derived from the sender + the tx's OWN nonce (alloy
            // `Address::create`, the geth `crypto.CreateAddress` port), only
            // set for a contract-creation tx (`to == None`).
            let contract_address = if tx.is_create() {
                Some(tx.signer().create(tx.nonce()))
            } else {
                None
            };

            // `tx_index` is an RPC-only ordinal (not consensus-critical, and a
            // block can never realistically carry `u64::MAX` txs), so this
            // saturates rather than erroring — unlike `gas_used`'s
            // `checked_sub` above, an overflow here would not indicate a
            // corrupted receipt list, so `Error::FeeOverflow` would misuse
            // that sentinel (review fix, minor 2).
            let tx_index = u64::try_from(idx).unwrap_or(u64::MAX);

            let record = TxReceiptRecord {
                tx_hash,
                block_hash,
                block_number,
                tx_index,
                from: tx.signer(),
                to: tx.to(),
                contract_address,
                gas_used,
                cumulative_gas_used: receipt.cumulative_gas_used,
                // alloy `Transaction::effective_gas_price`: `gas_price` for a
                // legacy tx, `min(max_fee_per_gas, base_fee +
                // max_priority_fee_per_gas)` for a dynamic-fee tx — the same
                // formula coreth/geth's `types.Transaction` uses for
                // `eth_getTransactionReceipt`'s `effectiveGasPrice`.
                effective_gas_price: tx.effective_gas_price(base_fee),
                success: receipt.success,
                logs: receipt.logs.clone(),
                tx_type: receipt.ty(),
                first_log_index,
            };

            // Write the KV row THEN record into the in-memory index for the
            // SAME tx, so a failure here never leaves a `tx_number` row
            // without a matching `AcceptedTxIndex` entry (or vice versa).
            ctx.canonical.put_tx_number(tx_hash, block_number)?;
            ctx.accepted_tx_index.record(vec![record]);
        }
        Ok(())
    }

    /// **Reject** (spec 10 §3.1): drop the uncommitted Firewood proposal for
    /// `precommit_root`. No state was committed, so reject is cheap and writes
    /// nothing to the canonical store. Siblings hold independent proposals
    /// (proposal-on-proposal, 04 §4.2), so dropping one never disturbs another.
    ///
    /// # Errors
    /// Infallible today (returns [`Result`] to match the lifecycle signature and
    /// the spec-06 trait shape).
    pub fn reject(&self, ctx: &EvmBlockContext, precommit_root: B256) -> Result<()> {
        // A rejected block's `SendWarpMessage` logs are never routed (spec 20
        // §3.1 — only accepted messages are signed).
        if let Some(seam) = ctx.warp.as_ref() {
            seam.pending.lock().remove(&precommit_root);
        }
        // Likewise its stashed receipts (cchain-tx-pipeline task 3): a rejected
        // block's receipts are never persisted, so the stash entry must not
        // leak into a later accept of a different block that happens to reuse
        // this pre-commit root value (never expected under G1, but cheap to
        // guarantee).
        ctx.receipts.lock().remove(&precommit_root);
        ctx.state.discard(precommit_root);
        Ok(())
    }
}

/// Decodes Go-produced (coreth) C-Chain block bytes into an
/// [`EvmBlock::Unverified`] (spec 10 §9.3 / §6.2).
///
/// The block list is `[Header, Txs, Uncles, Version, ExtData]`; the atomic txs
/// are extracted from `ExtData` (fork-gated: AP5+ uses the *batch* encoding —
/// pre-AP5 single-tx blocks predate this VM port and are not produced here). The
/// recovered block ID = `keccak256(header RLP)`.
///
/// # Errors
/// Returns [`Error`] if the bytes are not valid coreth block RLP, if there are
/// trailing bytes, or if `ExtData` fails to decode into atomic txs.
pub fn decode_ava_evm_block(bytes: &[u8], spec: &AvaChainSpec) -> Result<EvmBlock> {
    let mut buf: &[u8] = bytes;

    // Outer block list header.
    let list = RlpListHeader::decode(&mut buf).map_err(rlp_err)?;
    if !list.list {
        return Err(rlp_err(RlpError::UnexpectedString));
    }
    if list.payload_length > buf.len() {
        return Err(rlp_err(RlpError::InputTooShort));
    }
    let (payload_bytes, rest) = buf.split_at(list.payload_length);
    if !rest.is_empty() {
        return Err(rlp_err(RlpError::UnexpectedLength));
    }
    let mut payload = payload_bytes;
    let body = &mut payload;

    // 1) Header (coreth extras).
    let header = AvaHeader::decode_rlp(body)?;

    // 2) Txs — a list of EIP-2718 typed-envelope items.
    let transactions = decode_tx_list(body)?;

    // 3) Uncles — always empty on the C-Chain; a non-empty list is rejected.
    decode_uncle_list(body)?;

    // 4) Version (uint32).
    let version = u32::decode(body).map_err(rlp_err)?;

    // 5) ExtData (bytes; carries the atomic txs).
    let ext_data = Bytes::decode(body).map_err(rlp_err)?.to_vec();

    if !body.is_empty() {
        return Err(rlp_err(RlpError::UnexpectedLength));
    }

    // Extract atomic txs from ExtData (fork-gated, §6.2).
    let atomic_txs = extract_atomic_txs(&ext_data, &header, spec)?;

    let hash = header.hash();
    let parts = AvaBlockParts {
        header,
        transactions,
        atomic_txs,
        ext_data,
        version,
    };
    Ok(EvmBlock::Unverified(EvmBlockInner {
        parts,
        encoded: bytes.to_vec(),
        hash,
    }))
}

/// Re-assembles a C-Chain block from its parts into the **byte-identical**
/// coreth wire form (spec 10 §9.3) and returns it as an [`EvmBlock::Built`].
///
/// The reverse of [`decode_ava_evm_block`]: encodes
/// `[Header, Txs, Uncles(empty), Version, ExtData]`. The caller is responsible
/// for having populated `ext_data` consistently with `header.ext_data_hash`
/// (the builder, M6.20, computes both); this function does not recompute it.
///
/// # Errors
/// Returns [`Error`] if assembly fails. (None of the current paths fail, but the
/// signature is fallible for the builder's future use.)
pub fn assemble_ava_block(parts: AvaBlockParts, _spec: &AvaChainSpec) -> Result<EvmBlock> {
    // Build the inner payload, then frame the outer list.
    let mut payload = Vec::new();
    parts.header.encode_rlp(&mut payload);
    encode_tx_list(&parts.transactions, &mut payload);
    encode_empty_list(&mut payload); // uncles (always empty on the C-Chain)
    parts.version.encode(&mut payload);
    Bytes::from(parts.ext_data.clone()).encode(&mut payload);

    let mut encoded = Vec::new();
    RlpListHeader {
        list: true,
        payload_length: payload.len(),
    }
    .encode(&mut encoded);
    encoded.extend_from_slice(&payload);

    let hash = parts.header.hash();
    Ok(EvmBlock::Built(EvmBlockInner {
        parts,
        encoded,
        hash,
    }))
}

// ---------------------------------------------------------------------------
// Atomic-tx extraction (§6.2) — coreth `atomic.ExtractAtomicTxs`
// ---------------------------------------------------------------------------

/// Extracts the atomic txs carried in `ext_data` (coreth
/// `atomic.ExtractAtomicTxs`). Post-ApricotPhase5 the encoding is a *batch*
/// (`Codec.Marshal(0, []*Tx)`); pre-AP5 it was a single tx. Blocks produced by
/// this VM are AP5+, so we use the batch decoder when AP5 is active and the
/// single-tx decoder otherwise.
fn extract_atomic_txs(
    ext_data: &[u8],
    header: &AvaHeader,
    spec: &AvaChainSpec,
) -> Result<Vec<AtomicTx>> {
    if ext_data.is_empty() {
        return Ok(Vec::new());
    }
    let batch = spec.fork_at(header.time) >= AvaPhase::ApricotPhase5;
    let mut txs: Vec<AtomicTx> = if batch {
        // AP5+ batch: `Codec.Marshal(0, []*Tx)`. The codec's `Vec<T>` decoder
        // requires `T: Deserializable + Default` — `AtomicTx::Tx` satisfies both.
        let mut decoded: Vec<AtomicTx> = Vec::new();
        atomic_codec()
            .unmarshal(ext_data, &mut decoded)
            .map_err(|_| Error::NilTx)?;
        decoded
    } else {
        // Pre-AP5 single tx.
        let mut tx = AtomicTx::default();
        atomic_codec()
            .unmarshal(ext_data, &mut tx)
            .map_err(|_| Error::NilTx)?;
        vec![tx]
    };
    // Re-derive each tx's cached signed bytes + id (coreth re-runs `Sign`).
    for tx in &mut txs {
        tx.initialize().map_err(|_| Error::NilTx)?;
    }
    Ok(txs)
}

// ---------------------------------------------------------------------------
// RLP list helpers for the block body
// ---------------------------------------------------------------------------

/// Decodes the `Txs` list — a list whose items are EIP-2718 typed envelopes.
fn decode_tx_list(buf: &mut &[u8]) -> Result<Vec<TransactionSigned>> {
    let list = RlpListHeader::decode(buf).map_err(rlp_err)?;
    if !list.list {
        return Err(rlp_err(RlpError::UnexpectedString));
    }
    if list.payload_length > buf.len() {
        return Err(rlp_err(RlpError::InputTooShort));
    }
    let (items_bytes, rest) = buf.split_at(list.payload_length);
    let mut items = items_bytes;
    let mut txs = Vec::new();
    while !items.is_empty() {
        let tx = TransactionSigned::decode_2718(&mut items).map_err(|_| Error::NilTx)?;
        txs.push(tx);
    }
    *buf = rest;
    Ok(txs)
}

/// Decodes the `Uncles` list; the C-Chain never has uncles (coreth
/// `wrapped_block.go:452-455`, `errUnclesUnsupported`), so a non-empty list is
/// rejected here at decode time — the Go-parity fix (M9.15 task L1): a
/// well-formed C-Chain block can only ever carry an empty uncle list, which is
/// what lets [`EvmBlock::syntactic_verify`]'s uncle-hash check collapse to a
/// single comparison against the empty-ommer sentinel.
fn decode_uncle_list(buf: &mut &[u8]) -> Result<()> {
    let list = RlpListHeader::decode(buf).map_err(rlp_err)?;
    if !list.list {
        return Err(rlp_err(RlpError::UnexpectedString));
    }
    if list.payload_length > buf.len() {
        return Err(rlp_err(RlpError::InputTooShort));
    }
    let (uncles, rest) = buf.split_at(list.payload_length);
    *buf = rest;
    if !uncles.is_empty() {
        return Err(rlp_err(RlpError::UnexpectedLength));
    }
    Ok(())
}

/// Encodes a `Txs` list (each tx as its EIP-2718 typed envelope).
fn encode_tx_list(txs: &[TransactionSigned], out: &mut Vec<u8>) {
    let mut payload = Vec::new();
    for tx in txs {
        // `Encodable` on `TransactionSigned` emits the form used inside a block
        // body (legacy: RLP list; typed: the 2718 envelope as an RLP byte
        // string), matching geth's block-body tx encoding.
        tx.encode(&mut payload);
    }
    RlpListHeader {
        list: true,
        payload_length: payload.len(),
    }
    .encode(out);
    out.extend_from_slice(&payload);
}

/// Encodes an empty RLP list (`0xc0`).
fn encode_empty_list(out: &mut Vec<u8>) {
    RlpListHeader {
        list: true,
        payload_length: 0,
    }
    .encode(out);
}

// ---------------------------------------------------------------------------
// Optional-field RLP scalar/uint64 helpers (Go `WriteBigInt`/`WriteUint64`)
// ---------------------------------------------------------------------------

/// Encodes an optional big-int scalar: `Some(v)` → minimal RLP scalar, `None` →
/// the empty string `0x80` (Go `w.Write(rlp.EmptyString)`).
fn encode_scalar_opt(v: Option<U256>, out: &mut Vec<u8>) {
    match v {
        Some(x) => x.encode(out),
        None => out.push(RLP_EMPTY_STRING_CODE),
    }
}

/// Encodes an optional `uint64`: `Some(v)` → minimal RLP scalar, `None` →
/// `0x80` (Go `w.Write([]byte{0x80})`).
fn encode_u64_opt(v: Option<u64>, out: &mut Vec<u8>) {
    match v {
        Some(x) => x.encode(out),
        None => out.push(RLP_EMPTY_STRING_CODE),
    }
}

/// Decodes one optional big-int scalar if bytes remain.
fn decode_scalar_opt(buf: &mut &[u8]) -> Result<Option<U256>> {
    if buf.is_empty() {
        return Ok(None);
    }
    Ok(Some(U256::decode(buf).map_err(rlp_err)?))
}

/// Decodes one optional `uint64` if bytes remain.
fn decode_u64_opt(buf: &mut &[u8]) -> Result<Option<u64>> {
    if buf.is_empty() {
        return Ok(None);
    }
    Ok(Some(u64::decode(buf).map_err(rlp_err)?))
}

/// Decodes one optional `B256` if bytes remain.
fn decode_b256_opt(buf: &mut &[u8]) -> Result<Option<B256>> {
    if buf.is_empty() {
        return Ok(None);
    }
    // A later optional field (Granite time-ms / min-delay-excess) present with
    // this one nil forces coreth to emit an empty-string placeholder (`0x80`)
    // in the pointer slot; Go's `rlp:"optional"` pointer decodes it back to nil.
    // Match that: consume the empty string and yield `None` rather than failing
    // `B256::decode` (which expects 32 bytes). A block with this shape is then
    // cleanly rejected by the Cancun clamp with the coreth `errMissingParentBeaconRoot`,
    // exactly as Go rejects it — rather than erroring one layer too early.
    if buf[0] == RLP_EMPTY_STRING_CODE {
        *buf = &buf[1..];
        return Ok(None);
    }
    Ok(Some(B256::decode(buf).map_err(rlp_err)?))
}

/// Maps an `alloy_rlp` decode error onto the crate error model (no `reth_*`
/// type names leak — the facade hands us [`RlpError`]).
fn rlp_err(_e: RlpError) -> Error {
    Error::NilTx
}

/// The canonical empty-`ExtData` hash (`customtypes.EmptyExtDataHash`).
#[must_use]
pub fn empty_ext_data_hash() -> B256 {
    B256::from(EMPTY_EXT_DATA_HASH)
}

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;
    use ava_evm_reth::{EvmSignature, SignableTransaction, TxLegacy};

    use super::*;
    use crate::chainspec::NetworkUpgrades;

    /// A `NetworkUpgrades` schedule with AP1/AP3/AP4 activated at the given
    /// unix seconds (`u64::MAX` == never); every later phase (AP5..Granite,
    /// Helicon) is parked far in the future so the header stays pre-Etna (the
    /// Cancun clamp then requires the blob/beacon-root fields to stay
    /// `None`, which the test headers below already satisfy).
    fn upgrades(ap1: u64, ap3: u64, ap4: u64) -> NetworkUpgrades {
        const FAR_FUTURE: u64 = u64::MAX;
        NetworkUpgrades {
            apricot_phase_1: ap1,
            apricot_phase_2: ap1,
            apricot_phase_3: ap3,
            apricot_phase_4: ap4,
            apricot_phase_5: FAR_FUTURE,
            apricot_phase_pre_6: FAR_FUTURE,
            apricot_phase_6: FAR_FUTURE,
            apricot_phase_post_6: FAR_FUTURE,
            banff: FAR_FUTURE,
            cortina: FAR_FUTURE,
            durango: FAR_FUTURE,
            etna: FAR_FUTURE,
            fortuna: FAR_FUTURE,
            granite: FAR_FUTURE,
            helicon: FAR_FUTURE,
        }
    }

    /// A self-consistent, minimal test header: every check `syntactic_verify`
    /// runs *before* the one under test in a given case is satisfied
    /// (difficulty/mixDigest/nonce/version zero-or-one, uncleHash/coinbase
    /// correct, `tx_root` derived from `transactions`), so a case only needs
    /// to vary the field(s) its check inspects. `extra` must be sized for the
    /// caller's active phase (`VerifyExtra` now runs before every check this
    /// helper is used for) — callers pass a phase-appropriate length.
    fn test_header(
        transactions: &[TransactionSigned],
        base_fee: Option<U256>,
        block_gas_cost: Option<U256>,
        extra: Bytes,
    ) -> AvaHeader {
        AvaHeader {
            parent_hash: B256::ZERO,
            uncle_hash: EMPTY_OMMER_ROOT_HASH,
            coinbase: BLACKHOLE_ADDRESS,
            state_root: B256::ZERO,
            tx_root: calculate_transaction_root(transactions),
            receipt_root: B256::ZERO,
            bloom: Bytes::from(vec![0u8; 256]),
            difficulty: U256::from(1),
            number: 1,
            gas_limit: 8_000_000,
            gas_used: 0,
            time: 0,
            extra,
            mix_digest: B256::ZERO,
            nonce: [0u8; 8],
            ext_data_hash: empty_ext_data_hash(),
            base_fee,
            ext_data_gas_used: None,
            block_gas_cost,
            blob_gas_used: None,
            excess_blob_gas: None,
            parent_beacon_root: None,
            time_milliseconds: None,
            min_delay_excess: None,
        }
    }

    /// Assembles `header` + `transactions` into an [`EvmBlock`] via the same
    /// path the wire codec uses, so `syntactic_verify` sees a block shaped
    /// exactly as `decode_ava_evm_block` would hand it one.
    fn build_block(header: AvaHeader, transactions: Vec<TransactionSigned>) -> EvmBlock {
        let parts = AvaBlockParts {
            header,
            transactions,
            atomic_txs: Vec::new(),
            ext_data: Vec::new(),
            version: 0,
        };
        assemble_ava_block(
            parts,
            &AvaChainSpec::c_chain(1, ava_evm_reth::Chain::from_id(43112)),
        )
        .expect("assemble test block")
    }

    /// A signed legacy tx with the given `gas_price` (signature is bogus —
    /// `syntactic_verify` never recovers senders, only reads `GasPrice()`).
    fn legacy_tx(gas_price: u128) -> TransactionSigned {
        let tx = TxLegacy {
            chain_id: Some(43112),
            nonce: 0,
            gas_price,
            gas_limit: 21_000,
            to: ava_evm_reth::TxKind::Call(Address::ZERO),
            value: U256::ZERO,
            input: Bytes::new(),
        };
        let sig = EvmSignature::new(U256::from(1), U256::from(1), false);
        TransactionSigned::Legacy(tx.into_signed(sig))
    }

    /// coreth `wrapped_block.go:476-483`: `BaseFee == nil` at AP3+ is
    /// rejected with `errNilBaseFeeApricotPhase3`.
    #[test]
    fn nil_base_fee_at_apricot_phase3_is_rejected() {
        let spec = AvaChainSpec::from_parts(
            upgrades(0, 0, u64::MAX),
            ava_evm_reth::Chain::from_id(43112),
            false,
        );
        // Phase is ApricotPhase3 (ap3=0, durango=FAR_FUTURE): VerifyExtra's
        // ApricotPhase3 arm requires extra_len == window::WINDOW_SIZE (80).
        let header = test_header(&[], None, None, Bytes::from(vec![0u8; window::WINDOW_SIZE]));
        let block = build_block(header, Vec::new());
        let err = block
            .syntactic_verify(&spec)
            .expect_err("nil BaseFee at AP3+ must be rejected");
        assert_matches!(err, Error::NilBaseFee);
    }

    /// coreth `wrapped_block.go:486-495`: `BlockGasCost == nil` at AP4+ is
    /// rejected with `errNilBlockGasCostApricotPhase4`.
    #[test]
    fn nil_block_gas_cost_at_apricot_phase4_is_rejected() {
        let spec = AvaChainSpec::from_parts(
            upgrades(0, 0, 0),
            ava_evm_reth::Chain::from_id(43112),
            false,
        );
        // AP3 is active too (AP4 implies AP3 chronologically), so BaseFee
        // must be present for that earlier check to pass. Phase is
        // ApricotPhase4 (durango=FAR_FUTURE): VerifyExtra's ApricotPhase3 arm
        // (the highest one that matches) requires extra_len == 80.
        let header = test_header(
            &[],
            Some(U256::ZERO),
            None,
            Bytes::from(vec![0u8; window::WINDOW_SIZE]),
        );
        let block = build_block(header, Vec::new());
        let err = block
            .syntactic_verify(&spec)
            .expect_err("nil BlockGasCost at AP4+ must be rejected");
        assert_matches!(err, Error::InvalidBlockGasCost(None));
    }

    /// coreth `wrapped_block.go:458-465`: pre-ApricotPhase1, every tx's
    /// `GasPrice()` must be >= `ap0.MinGasPrice` (470 gwei).
    #[test]
    fn gas_price_below_ap0_minimum_is_rejected() {
        let spec = AvaChainSpec::from_parts(
            upgrades(u64::MAX, u64::MAX, u64::MAX),
            ava_evm_reth::Chain::from_id(43112),
            false,
        );
        let low_price = AP0_MIN_GAS_PRICE - 1;
        let tx = legacy_tx(low_price);
        // Pre-ApricotPhase1: VerifyExtra's default arm requires
        // extra_len <= AP0_MAX_EXTRA_DATA_SIZE (64); empty extra satisfies it.
        let header = test_header(std::slice::from_ref(&tx), None, None, Bytes::new());
        let block = build_block(header, vec![tx]);
        let err = block
            .syntactic_verify(&spec)
            .expect_err("gas price below ap0.MinGasPrice must be rejected");
        assert_matches!(
            err,
            Error::GasPriceTooLow { have, min, .. }
                if have == low_price && min == AP0_MIN_GAS_PRICE
        );
    }

    /// `decode_uncle_list` (the Go-parity fix, `wrapped_block.go:452-455`):
    /// a non-empty uncle list must be rejected at wire decode, not silently
    /// admitted and stripped.
    #[test]
    fn decode_uncle_list_rejects_non_empty_list() {
        // `[0xc1, 0x80]` — a one-item RLP list whose single element is the
        // empty string (`0x80`); a well-formed (if bogus) single-uncle list.
        let mut buf: &[u8] = &[0xc1, 0x80];
        let err = decode_uncle_list(&mut buf).expect_err("non-empty uncle list must be rejected");
        assert_matches!(err, Error::NilTx);
    }

    /// `decode_uncle_list` still accepts the empty list (`0xc0`) — the only
    /// shape a well-formed C-Chain block ever carries.
    #[test]
    fn decode_uncle_list_accepts_empty_list() {
        let mut buf: &[u8] = &[0xc0];
        decode_uncle_list(&mut buf).expect("empty uncle list must decode");
        assert!(buf.is_empty(), "the empty list's bytes must be consumed");
    }
}
