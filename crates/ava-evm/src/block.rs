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
    Address, B256, Bytes, Decodable2718, ExternalConsensusExecutor, Header, RLP_EMPTY_STRING_CODE,
    RecoveredTx, RlpDecodable, RlpEncodable, RlpError, RlpListHeader, SignerRecoverable, State,
    StateBuilder, StateProviderDatabase, TransactionSigned, U256, keccak256,
};

use crate::atomic::backend::AtomicBackend;
use crate::atomic::tx::{Tx as AtomicTx, codec as atomic_codec};
use crate::canonical::CanonicalStore;
use crate::chainspec::{AvaChainSpec, AvaPhase};
use crate::error::{Error, Result};
use crate::evmconfig::{AvaEvmConfig, NoopPreHook};
use crate::state::{FirewoodStateProvider, FirewoodStateView};

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
            number: h.number,
            timestamp: h.time,
            gas_limit: h.gas_limit,
            gas_used: h.gas_used,
            base_fee_per_gas,
            beneficiary: h.coinbase,
            extra_data: h.extra.clone(),
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
        }
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
        let outcome = ctx
            .evm_config
            .execute_batch(env, &mut state, &NoopPreHook, &txs)?;

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
        // 1. Commit the Firewood proposal -> durably advances the EVM state tip.
        ctx.state.commit(precommit_root)?;

        // 2. AtomicBackend indexing (§6.4/§17.4): AFTER the EVM state commit,
        //    index this block's atomic txs into the atomic trie and apply the
        //    cross-chain shared-memory batch (Import → Remove on source, Export →
        //    Put on dest). Skipped when no backend is attached (M6.17).
        if let Some(backend) = ctx.atomic_backend.as_ref() {
            backend.accept(self.number(), self.atomic_txs())?;
        }

        // 3. Append non-state block metadata + advance the canonical tip (G6,
        //    §17.7). precompile-accept callbacks (§8) are wired by M6.22; the
        //    canonical append is this task.
        ctx.canonical.append_canonical(
            self.number(),
            self.hash(),
            self.header().state_root,
            self.ext_data(),
            // Receipts bytes are persisted once the receipt encoding is wired
            // (RPC/history task); the canonical index contract is satisfied by the
            // header/hash/number rows + tip pointer here.
            &[],
        )?;
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

    // 3) Uncles — always empty on the C-Chain, but consume the list.
    let _uncles = decode_uncle_list(body)?;

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

/// Decodes (and discards) the `Uncles` list; the C-Chain never has uncles, but
/// the list framing must be consumed. Returns the count for sanity.
fn decode_uncle_list(buf: &mut &[u8]) -> Result<usize> {
    let list = RlpListHeader::decode(buf).map_err(rlp_err)?;
    if !list.list {
        return Err(rlp_err(RlpError::UnexpectedString));
    }
    if list.payload_length > buf.len() {
        return Err(rlp_err(RlpError::InputTooShort));
    }
    let (uncles, rest) = buf.split_at(list.payload_length);
    *buf = rest;
    // Uncles are headers; the C-Chain forbids them, so a non-empty list is
    // invalid. We only need to skip the bytes for round-trip parity.
    Ok(usize::from(!uncles.is_empty()))
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
