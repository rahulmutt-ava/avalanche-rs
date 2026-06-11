// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The Warp stateful precompile + the pre-execution predicate pass (G4, spec 10
//! §6.5/§8/§17.5, spec 20 §7).
//!
//! ## What lives here
//!
//! - [`WarpPrecompile`] — the [`StatefulPrecompile`] at
//!   [`WARP_PRECOMPILE_ADDRESS`] (`0x02..05`), dispatching the four ABI selectors
//!   (spec 20 §7.1): `getBlockchainID`, `sendWarpMessage`,
//!   `getVerifiedWarpMessage`, `getVerifiedWarpBlockHash`. It charges gas from the
//!   fork-selected [`GasConfig`] table ([`PRE_GRANITE_GAS_CONFIG`] /
//!   [`GRANITE_GAS_CONFIG`], spec 20 §7.3), reads the verified predicate results
//!   threaded into the [`PrecompileCtx`] (it never re-verifies BLS at call time),
//!   and records the `SendWarpMessage` logs it emits (the warp backend / accept
//!   hook drains them, spec 20 §3.1).
//! - [`run_predicates`] — the **predicate pass** (spec 20 §7.2): for each warp
//!   message attached to a transaction's access list, parse it, resolve the
//!   source subnet (+ the `requirePrimaryNetworkSigners` substitution branch),
//!   and BLS-aggregate-verify it against the source subnet's [`WarpSet`] at the
//!   proposervm-pinned P-Chain height, producing a `Vec<bool>`. It runs in
//!   `apply_pre_execution_changes` and stashes results into
//!   [`PredicateResults`].
//! - [`WarpBackend`] / [`WarpPrecompile::take_logs`] — `handlePrecompileAccept`
//!   (spec 20 §3.1): on block accept, the unsigned messages from the
//!   `SendWarpMessage` logs are recorded so the node will sign them.
//!
//! ## Predicate encoding (coreth `vms/evm/predicate`)
//!
//! A warp message rides in a transaction's access list as a sequence of 32-byte
//! storage-key "chunks": the raw message bytes, a `0xff` delimiter byte, then
//! zero-padding to a 32-byte multiple ([`predicate_to_chunks`] /
//! [`predicate_from_chunks`]). The per-chunk gas (`PerWarpMessageChunk *
//! num_chunks`) is charged over the chunk count, not the unpadded length.
//!
//! ## ABI (no `alloy`/`ethabi` dependency)
//!
//! The four selectors + the `SendWarpMessage` event have fixed, simple shapes, so
//! the (de)serialization is done by hand against the canonical Solidity ABI
//! layout (head words + dynamic tails). The selectors / event topic are pinned in
//! `tests/vectors/cchain/warp/selectors.json`.

use std::sync::Arc;

use ava_evm_reth::{
    Address, B256, Bytes, Gas, InstructionResult, InterpreterResult, PrecompileError, U256,
};
use ava_types::constants::PRIMARY_NETWORK_ID;
use ava_types::id::Id;
use ava_utils::bits::Bits;
use ava_validators::state::ValidatorState;
use ava_warp::payload::{AddressedCall, Hash, WarpPayload};
use ava_warp::verifier::{
    WARP_QUORUM_DENOMINATOR, WARP_QUORUM_NUMERATOR, verify_bit_set_signature,
};
use ava_warp::{Message, Signature, UnsignedMessage};
use parking_lot::Mutex;

use crate::precompile::registry::{PrecompileCtx, PrecompileStateOps, StatefulPrecompile};

/// `ContractAddress` — the warp precompile address `0x02..05` (`module.go`).
pub const WARP_PRECOMPILE_ADDRESS: Address = Address::new([
    0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x05,
]);

/// `WarpDefaultQuorumNumerator` (spec 20 §6.2). `quorum_numerator == 0` means use
/// this default.
pub const WARP_DEFAULT_QUORUM_NUMERATOR: u64 = WARP_QUORUM_NUMERATOR;
/// `WarpQuorumNumeratorMinimum` — the minimum configurable numerator (spec 20 §6.2).
pub const WARP_QUORUM_NUMERATOR_MINIMUM: u64 = 33;
/// `WarpQuorumDenominator` (spec 20 §6.2).
pub const WARP_QUORUM_DENOMINATOR_CONST: u64 = WARP_QUORUM_DENOMINATOR;

// ---- ABI selectors (4-byte keccak prefixes, spec 20 §7.1) -----------------

/// `getBlockchainID()` selector.
const SEL_GET_BLOCKCHAIN_ID: [u8; 4] = [0x42, 0x13, 0xcf, 0x78];
/// `sendWarpMessage(bytes)` selector.
const SEL_SEND_WARP_MESSAGE: [u8; 4] = [0xee, 0x5b, 0x48, 0xeb];
/// `getVerifiedWarpMessage(uint32)` selector.
const SEL_GET_VERIFIED_WARP_MESSAGE: [u8; 4] = [0x6f, 0x82, 0x53, 0x50];
/// `getVerifiedWarpBlockHash(uint32)` selector.
const SEL_GET_VERIFIED_WARP_BLOCK_HASH: [u8; 4] = [0xce, 0x7f, 0x59, 0x29];

/// `keccak256("SendWarpMessage(address,bytes32,bytes)")` — the event topic0.
const SEND_WARP_MESSAGE_EVENT_TOPIC: [u8; 32] = [
    0x56, 0x60, 0x0c, 0x56, 0x77, 0x28, 0xa8, 0x00, 0xc0, 0xaa, 0x92, 0x75, 0x00, 0xf8, 0x31, 0xcb,
    0x45, 0x1d, 0xf6, 0x6a, 0x7a, 0xf5, 0x70, 0xeb, 0x4d, 0xf4, 0xdf, 0xbf, 0x46, 0x74, 0x88, 0x7d,
];

/// `addWarpMessageBaseGasCost` — the cost of producing/serving a BLS signature
/// (coreth `contract.go`).
const ADD_WARP_MESSAGE_BASE_GAS_COST: u64 = 20_000;
/// `contract.LogGas` (geth `params/protocol_params.go`).
const LOG_GAS: u64 = 375;
/// `contract.LogTopicGas`.
const LOG_TOPIC_GAS: u64 = 375;
/// `contract.LogDataGas` (per byte).
const LOG_DATA_GAS: u64 = 8;
/// `contract.WriteGasCostPerSlot`.
const WRITE_GAS_COST_PER_SLOT: u64 = 20_000;

/// `sendWarpMessageBase` — base log + 3 topics + BLS-sig + trie-write
/// (coreth `contract.go`). Unchanged across forks.
const SEND_WARP_MESSAGE_BASE: u64 =
    LOG_GAS + 3 * LOG_TOPIC_GAS + ADD_WARP_MESSAGE_BASE_GAS_COST + WRITE_GAS_COST_PER_SLOT;

/// The warp gas-cost table (coreth `warp/contract.go::GasConfig`, spec 20 §7.3).
/// Two instances exist — [`PRE_GRANITE_GAS_CONFIG`] and [`GRANITE_GAS_CONFIG`] —
/// selected by the active fork.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GasConfig {
    /// Cost to call `getBlockchainID`.
    pub get_blockchain_id: u64,
    /// Base cost of entering `getVerifiedWarpMessage` / `getVerifiedWarpBlockHash`.
    pub get_verified_warp_message_base: u64,
    /// Gas per warp signer in the validator set (predicate verification).
    pub per_warp_signer: u64,
    /// Gas per 32-byte chunk of the warp message.
    pub per_warp_message_chunk: u64,
    /// Base cost to verify a warp predicate (BLS signature).
    pub verify_predicate_base: u64,
    /// Base cost of entering `sendWarpMessage`.
    pub send_warp_message_base: u64,
    /// Per-byte cost of producing a sent message of a given size.
    pub per_warp_message_byte: u64,
}

/// `preGraniteGasConfig` (coreth `warp/contract.go`).
pub const PRE_GRANITE_GAS_CONFIG: GasConfig = GasConfig {
    get_blockchain_id: 2,
    get_verified_warp_message_base: 2,
    per_warp_signer: 500,
    per_warp_message_chunk: 3_200,
    verify_predicate_base: 200_000,
    send_warp_message_base: SEND_WARP_MESSAGE_BASE,
    per_warp_message_byte: LOG_DATA_GAS,
};

/// `graniteGasConfig` (coreth `warp/contract.go`): raised read/verify costs to
/// target a worst-case verification cost (~100 mgas/s after epoching).
pub const GRANITE_GAS_CONFIG: GasConfig = GasConfig {
    get_blockchain_id: 200,
    get_verified_warp_message_base: 750,
    per_warp_signer: 250,
    per_warp_message_chunk: 512,
    verify_predicate_base: 125_000,
    send_warp_message_base: SEND_WARP_MESSAGE_BASE,
    per_warp_message_byte: LOG_DATA_GAS,
};

/// `CurrentGasConfig(rules)` — the gas table for the active fork (spec 20 §7.3).
#[must_use]
pub fn current_gas_config(is_granite: bool) -> GasConfig {
    if is_granite {
        GRANITE_GAS_CONFIG
    } else {
        PRE_GRANITE_GAS_CONFIG
    }
}

/// A log the warp precompile emitted (`SendWarpMessage`). Mirrors coreth's
/// `types.Log` for the warp use: the precompile address, the indexed topics, and
/// the ABI-encoded data (the unsigned-message bytes). The block-accept
/// `handlePrecompileAccept` hook re-parses `data` into an [`UnsignedMessage`] and
/// records it in the [`WarpBackend`] (spec 20 §3.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WarpLog {
    /// The emitting precompile address ([`WARP_PRECOMPILE_ADDRESS`]).
    pub address: Address,
    /// `[event_topic, sender, message_id]`.
    pub topics: Vec<B256>,
    /// The ABI-encoded `bytes message` (the unsigned-message wire bytes).
    pub data: Vec<u8>,
}

/// The Warp stateful precompile (spec 20 §7.1). Cloning shares the emitted-log
/// buffer (the executor drains it after the block; the hook records the messages
/// on accept).
#[derive(Clone)]
pub struct WarpPrecompile {
    /// This chain's blockchain id (the snow ctx `ChainID`).
    chain_id: Id,
    /// The verifying node's network id.
    network_id: u32,
    /// Whether Granite is active (selects the gas table).
    is_granite: bool,
    /// The `SendWarpMessage` logs emitted during this block (shared so the
    /// executor can drain them after execution).
    logs: Arc<Mutex<Vec<WarpLog>>>,
}

impl WarpPrecompile {
    /// Builds the warp precompile for a block: `chain_id`/`network_id` from the
    /// snow context, `is_granite` from the active fork (selects the gas table).
    #[must_use]
    pub fn new(chain_id: Id, network_id: u32, is_granite: bool) -> Self {
        Self {
            chain_id,
            network_id,
            is_granite,
            logs: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Builds the registry [`crate::precompile::registry::PrecompileModule`] for
    /// the warp precompile, activated at `activation` (the warp upgrade
    /// timestamp, spec 10 §8.3). The integration seam
    /// [`crate::evmconfig::AvaEvmConfig::with_precompiles`] registers this into the
    /// height-gated provider.
    #[must_use]
    pub fn module(self, activation: u64) -> crate::precompile::registry::PrecompileModule {
        crate::precompile::registry::PrecompileModule {
            address: WARP_PRECOMPILE_ADDRESS,
            activation,
            precompile: Arc::new(self),
        }
    }

    /// The active fork's [`GasConfig`].
    #[must_use]
    pub fn gas_config(&self) -> GasConfig {
        current_gas_config(self.is_granite)
    }

    /// Drains and returns the `SendWarpMessage` logs emitted so far (the executor
    /// calls this after the block; `handlePrecompileAccept` records them).
    #[must_use]
    pub fn take_logs(&self) -> Vec<WarpLog> {
        std::mem::take(&mut self.logs.lock())
    }

    /// `getBlockchainID()` — returns this chain's blockchain id (spec 20 §7.1).
    fn get_blockchain_id(&self, gas_limit: u64) -> Result<InterpreterResult, PrecompileError> {
        let gas = self.gas_config().get_blockchain_id;
        let mut g = Gas::new(gas_limit);
        if !g.record_regular_cost(gas) {
            return Ok(out_of_gas(gas_limit));
        }
        Ok(success(self.chain_id.as_bytes().to_vec(), g))
    }

    /// `sendWarpMessage(bytes payload)` — wraps `AddressedCall { source = caller,
    /// payload }` in an `UnsignedMessage`, emits the `SendWarpMessage` log, and
    /// returns the unsigned-message ID (spec 20 §7.1).
    fn send_warp_message(
        &self,
        input: &[u8],
        gas_limit: u64,
        ctx: &PrecompileCtx,
        state: &mut dyn PrecompileStateOps,
    ) -> Result<InterpreterResult, PrecompileError> {
        if ctx.read_only {
            // Go `vm.ErrWriteProtection` — a STATICCALL cannot emit the log.
            return Ok(failure(gas_limit));
        }
        let cfg = self.gas_config();
        let mut g = Gas::new(gas_limit);
        if !g.record_regular_cost(cfg.send_warp_message_base) {
            return Ok(out_of_gas(gas_limit));
        }
        // Charge for the size of the *input* (a conservative overestimate, before
        // unpacking the variable-sized argument), `SafeMul` (coreth `contract.go`).
        let Some(payload_gas) = cfg.per_warp_message_byte.checked_mul(input.len() as u64) else {
            return Ok(out_of_gas(gas_limit));
        };
        if !g.record_regular_cost(payload_gas) {
            return Ok(out_of_gas(gas_limit));
        }

        let Some(payload_data) = abi_decode_bytes(input) else {
            // A malformed input is a user-triggerable call failure (consumes the
            // supplied gas, geth error parity) — not a fatal EVM abort.
            return Ok(failure(gas_limit));
        };

        let call = AddressedCall {
            source_address: ctx.caller.as_slice().to_vec(),
            payload: payload_data,
        };
        let payload_bytes = WarpPayload::AddressedCall(call)
            .marshal_payload()
            .map_err(|e| PrecompileError::Fatal(format!("warp payload marshal: {e}")))?;
        let unsigned = UnsignedMessage {
            network_id: self.network_id,
            source_chain_id: self.chain_id,
            payload: payload_bytes,
        };
        let unsigned_bytes = unsigned
            .marshal()
            .map_err(|e| PrecompileError::Fatal(format!("warp message marshal: {e}")))?;
        let id = unsigned
            .id()
            .map_err(|e| PrecompileError::Fatal(format!("warp message id: {e}")))?;

        // Emit the `SendWarpMessage` log (3 topics + ABI-encoded message data):
        // into the live journal (so the receipt/bloom carry it, coreth
        // `stateDB.AddLog` parity — M6.31) AND into the shared side buffer the
        // accept hook drains (`take_logs` → `handle_precompile_accept`).
        let mut sender_topic = [0u8; 32];
        sender_topic[12..].copy_from_slice(ctx.caller.as_slice());
        let topics = vec![
            B256::from(SEND_WARP_MESSAGE_EVENT_TOPIC),
            B256::from(sender_topic),
            B256::from(*id.as_bytes()),
        ];
        let data = abi_encode_bytes(&unsigned_bytes);
        state.add_log(WARP_PRECOMPILE_ADDRESS, topics.clone(), data.clone());
        self.logs.lock().push(WarpLog {
            address: WARP_PRECOMPILE_ADDRESS,
            topics,
            data,
        });

        Ok(success(id.as_bytes().to_vec(), g))
    }

    /// `getVerifiedWarpMessage(uint32 index)` / `getVerifiedWarpBlockHash(uint32
    /// index)` — read the cached predicate result at `index`, charge the per-chunk
    /// read gas, and ABI-encode the parsed message (spec 20 §7.1, coreth
    /// `contract_warp_handler.go`).
    fn handle_warp_message(
        &self,
        input: &[u8],
        gas_limit: u64,
        ctx: &PrecompileCtx,
        kind: WarpReadKind,
    ) -> Result<InterpreterResult, PrecompileError> {
        let cfg = self.gas_config();
        let mut g = Gas::new(gas_limit);
        if !g.record_regular_cost(cfg.get_verified_warp_message_base) {
            return Ok(out_of_gas(gas_limit));
        }

        let Some(index) = abi_decode_u32(input) else {
            return Ok(failure(gas_limit));
        };

        let warp_preds = ctx
            .predicates
            .warp_for(ctx.block.current_tx_index, &WARP_PRECOMPILE_ADDRESS);
        let idx = index as usize;
        let valid = warp_preds
            .map(|p| idx < p.valid.len() && p.valid[idx])
            .unwrap_or(false);
        let pred_bytes = warp_preds.and_then(|p| p.predicates.get(idx));

        if !valid || pred_bytes.is_none() {
            return Ok(success(kind.pack_failed(), g));
        }
        let pred_chunks = pred_bytes.expect("checked is_none above");

        // Per-chunk read gas: charge over the number of 32-byte chunks (coreth
        // charges `PerWarpMessageChunk * len(pred)` where `len(pred)` is the chunk
        // count), `SafeMul`.
        let num_chunks = (pred_chunks.len() / 32) as u64;
        let Some(msg_bytes_gas) = cfg.per_warp_message_chunk.checked_mul(num_chunks) else {
            return Ok(out_of_gas(gas_limit));
        };
        if !g.record_regular_cost(msg_bytes_gas) {
            return Ok(out_of_gas(gas_limit));
        }

        // The predicate was verified before execution, so parsing should not
        // fail; coreth returns an error here (the call fails, consuming the
        // supplied gas) — mirror that as a non-fatal call failure.
        let Some(raw) = predicate_from_chunks(pred_chunks) else {
            return Ok(failure(gas_limit));
        };
        let Ok(msg) = Message::parse(&raw) else {
            return Ok(failure(gas_limit));
        };

        match kind.handle(&msg) {
            Ok(output) => Ok(success(output, g)),
            Err(_) => Ok(failure(gas_limit)),
        }
    }
}

/// Which `getVerified*` selector is being served (they share `handle_warp_message`).
#[derive(Clone, Copy)]
enum WarpReadKind {
    /// `getVerifiedWarpMessage` — parses an `AddressedCall` payload.
    AddressedPayload,
    /// `getVerifiedWarpBlockHash` — parses a `Hash` payload.
    BlockHash,
}

impl WarpReadKind {
    /// The ABI-encoded "invalid" output (the parsed message struct zeroed, `valid
    /// == false`).
    fn pack_failed(self) -> Vec<u8> {
        match self {
            WarpReadKind::AddressedPayload => {
                pack_verified_message(&[0u8; 32], &[0u8; 20], &[], false)
            }
            WarpReadKind::BlockHash => pack_verified_block_hash(&[0u8; 32], &[0u8; 32], false),
        }
    }

    /// Parse + ABI-encode the verified message (`valid == true`).
    fn handle(self, msg: &Message) -> Result<Vec<u8>, String> {
        match self {
            WarpReadKind::AddressedPayload => {
                let call = AddressedCall::parse(&msg.unsigned_message.payload)
                    .map_err(|e| format!("parse addressed payload: {e}"))?;
                Ok(pack_verified_message(
                    msg.unsigned_message.source_chain_id.as_bytes(),
                    &left20(&call.source_address),
                    &call.payload,
                    true,
                ))
            }
            WarpReadKind::BlockHash => {
                let WarpPayload::Hash(Hash { hash }) =
                    WarpPayload::parse(&msg.unsigned_message.payload)
                        .map_err(|e| format!("parse block-hash payload: {e}"))?
                else {
                    return Err("warp message payload is not a block hash".into());
                };
                Ok(pack_verified_block_hash(
                    msg.unsigned_message.source_chain_id.as_bytes(),
                    hash.as_bytes(),
                    true,
                ))
            }
        }
    }
}

impl StatefulPrecompile for WarpPrecompile {
    fn run(
        &self,
        input: &[u8],
        gas_limit: u64,
        ctx: &PrecompileCtx,
        state: &mut dyn PrecompileStateOps,
    ) -> Result<InterpreterResult, PrecompileError> {
        if input.len() < 4 {
            // Missing selector — a user-triggerable call failure (coreth's
            // `errInvalidSelector` consumes the supplied gas), not a fatal abort.
            return Ok(failure(gas_limit));
        }
        let mut selector = [0u8; 4];
        selector.copy_from_slice(&input[0..4]);
        let args = &input[4..];
        match selector {
            SEL_GET_BLOCKCHAIN_ID => self.get_blockchain_id(gas_limit),
            SEL_SEND_WARP_MESSAGE => self.send_warp_message(args, gas_limit, ctx, state),
            SEL_GET_VERIFIED_WARP_MESSAGE => {
                self.handle_warp_message(args, gas_limit, ctx, WarpReadKind::AddressedPayload)
            }
            SEL_GET_VERIFIED_WARP_BLOCK_HASH => {
                self.handle_warp_message(args, gas_limit, ctx, WarpReadKind::BlockHash)
            }
            _ => Ok(failure(gas_limit)),
        }
    }
}

// ---- The predicate pass (spec 20 §7.2) ------------------------------------

/// The proposervm-derived context the predicate pass verifies against (spec 20
/// §7.2). Sourced from the verifying block's proposervm block context +
/// the snow context (`Block::verify_with_context`).
#[derive(Clone, Copy, Debug)]
pub struct PredicateContext {
    /// The verifying node's network id (`SnowCtx.NetworkID`).
    pub network_id: u32,
    /// This chain's blockchain id (`SnowCtx.ChainID`).
    pub this_chain_id: Id,
    /// The local C-Chain subnet id (`SnowCtx.SubnetID`) — the substitution target
    /// in the `requirePrimaryNetworkSigners` branch.
    pub local_subnet_id: Id,
    /// The proposervm-pinned P-Chain height (`ProposerVMBlockCtx.PChainHeight`).
    pub pchain_height: u64,
    /// The per-deployment quorum numerator (`0` => [`WARP_DEFAULT_QUORUM_NUMERATOR`]).
    pub quorum_numerator: u64,
    /// Whether the deployment requires primary-network signers (spec 20 §7.2 step 3).
    pub require_primary_network_signers: bool,
}

impl PredicateContext {
    /// The effective quorum numerator (`0` resolves to the default 67).
    #[must_use]
    fn effective_quorum_numerator(&self) -> u64 {
        if self.quorum_numerator == 0 {
            WARP_DEFAULT_QUORUM_NUMERATOR
        } else {
            self.quorum_numerator
        }
    }
}

/// The predicate pass for one transaction's warp predicates (spec 20 §7.2,
/// subnet-evm `warp/config.go::VerifyPredicate`).
///
/// For each predicate (a chunked warp message in the access list), parse the
/// message, resolve the source subnet (applying the `requirePrimaryNetworkSigners`
/// substitution branch), and BLS-aggregate-verify it against the source subnet's
/// [`WarpSet`] at the proposervm-pinned P-Chain height. Returns one boolean per
/// predicate (`true` iff it verified) — the `Vec<bool>` the warp precompile reads
/// via [`crate::precompile::registry::PredicateResults::set_warp`].
///
/// A predicate whose bytes do not decode, whose message does not parse, whose
/// source subnet/validator set cannot be resolved, or whose signature does not
/// meet quorum reads as `false` (an invalid predicate is not a block error here —
/// `getVerifiedWarpMessage` simply returns `valid == false`).
///
/// # Errors
/// Returns a [`ValidatorState`] error only if the validator-state lookups
/// themselves fail (a node-level error, not a per-predicate verification failure).
pub async fn run_predicates<V: ValidatorState>(
    state: &V,
    ctx: &PredicateContext,
    predicates: &[Vec<u8>],
) -> Result<Vec<bool>, ava_validators::error::Error> {
    let mut results = Vec::with_capacity(predicates.len());
    for chunks in predicates {
        results.push(verify_one_predicate(state, ctx, chunks).await?);
    }
    Ok(results)
}

/// Verify a single warp predicate (spec 20 §7.2). Returns `Ok(true)` if it
/// verified, `Ok(false)` if it is structurally invalid or fails quorum, and `Err`
/// only on a validator-state lookup failure.
async fn verify_one_predicate<V: ValidatorState>(
    state: &V,
    ctx: &PredicateContext,
    chunks: &[u8],
) -> Result<bool, ava_validators::error::Error> {
    // 1. Decode the predicate chunks → raw message bytes → `Message`.
    let Some(raw) = predicate_from_chunks(chunks) else {
        return Ok(false);
    };
    let Ok(msg) = Message::parse(&raw) else {
        return Ok(false);
    };

    // 2. Resolve the source subnet (and the substitution branch, step 3).
    let source_chain_id = msg.unsigned_message.source_chain_id;
    let source_subnet = state.get_subnet_id(source_chain_id).await?;
    let source_subnet = if source_subnet == PRIMARY_NETWORK_ID
        && (!ctx.require_primary_network_signers || source_chain_id == platform_chain_id())
    {
        // The X-/C-chain may verify a primary-network message against the local,
        // likely smaller, subnet set. The P-chain is always trusted (always
        // synced), so a P-chain source always substitutes.
        ctx.local_subnet_id
    } else {
        source_subnet
    };

    // 3. Resolve the source subnet's `WarpSet` at the pinned P-Chain height.
    let sets = state.get_warp_validator_sets(ctx.pchain_height).await?;
    let Some(warp_set) = sets.get(&source_subnet) else {
        return Ok(false);
    };

    // 4. BLS-aggregate verify against the set (spec 20 §6).
    let Signature::BitSet(sig) = &msg.signature;
    let ok = verify_bit_set_signature(
        sig,
        &msg.unsigned_message,
        ctx.network_id,
        warp_set,
        ctx.effective_quorum_numerator(),
        WARP_QUORUM_DENOMINATOR,
    )
    .is_ok();
    Ok(ok)
}

/// `constants.PlatformChainID` — the P-Chain's blockchain id is the empty id
/// (Go `ids.Empty`), the same byte value as the primary network id.
fn platform_chain_id() -> Id {
    Id::EMPTY
}

// ---- The warp backend (handlePrecompileAccept, spec 20 §3.1) --------------

/// The warp backend that records unsigned messages the chain has produced
/// (coreth `warp.Backend.AddMessage`, spec 20 §3.1). On block accept,
/// [`handle_precompile_accept`] drains the `SendWarpMessage` logs into it so the
/// node will be willing to sign them.
#[derive(Clone, Default)]
pub struct WarpBackend {
    /// The recorded unsigned-message bytes, keyed by message id.
    messages: Arc<Mutex<std::collections::BTreeMap<Id, Vec<u8>>>>,
}

impl WarpBackend {
    /// A fresh, empty backend.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `Backend.AddMessage` — record one unsigned message (spec 20 §3.1).
    ///
    /// # Errors
    /// Returns the parse error if `unsigned_bytes` is not a valid unsigned message.
    pub fn add_message(&self, unsigned_bytes: &[u8]) -> Result<Id, ava_warp::Error> {
        let unsigned = UnsignedMessage::parse(unsigned_bytes).map_err(ava_warp::Error::Codec)?;
        let id = unsigned.id().map_err(ava_warp::Error::Codec)?;
        self.messages.lock().insert(id, unsigned_bytes.to_vec());
        Ok(id)
    }

    /// Whether a message with `id` has been recorded.
    #[must_use]
    pub fn contains(&self, id: &Id) -> bool {
        self.messages.lock().contains_key(id)
    }

    /// The number of recorded messages.
    #[must_use]
    pub fn len(&self) -> usize {
        self.messages.lock().len()
    }

    /// Whether the backend has recorded no messages.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.messages.lock().is_empty()
    }
}

/// `handlePrecompileAccept` (spec 20 §3.1, subnet-evm `warp/config.go::Accept`):
/// on block accept, parse each `SendWarpMessage` log's data into an unsigned
/// message and record it in the warp backend so the node will sign it.
///
/// # Errors
/// Returns the first parse error if a log's data is not a valid ABI-encoded
/// unsigned message.
pub fn handle_precompile_accept(
    backend: &WarpBackend,
    logs: &[WarpLog],
) -> Result<(), ava_warp::Error> {
    for log in logs {
        if log.address != WARP_PRECOMPILE_ADDRESS {
            continue;
        }
        if log.topics.first().map(B256::as_slice) != Some(&SEND_WARP_MESSAGE_EVENT_TOPIC) {
            continue;
        }
        // The log data is `abi.encode(bytes message)`; decode to the raw bytes.
        let Some(unsigned_bytes) = abi_decode_bytes(&log.data) else {
            return Err(ava_warp::Error::InvalidPayload);
        };
        backend.add_message(&unsigned_bytes)?;
    }
    Ok(())
}

// ---- Predicate chunk encoding (coreth `vms/evm/predicate`) ----------------

/// The predicate delimiter byte (coreth `predicate.go`).
const PREDICATE_DELIMITER: u8 = 0xff;

/// `predicate.New(b)` — chunk raw bytes into 32-byte words: append a `0xff`
/// delimiter then zero-pad to a 32-byte multiple (coreth `predicate.go`).
#[must_use]
pub fn predicate_to_chunks(b: &[u8]) -> Vec<u8> {
    let mut out = b.to_vec();
    out.push(PREDICATE_DELIMITER);
    let rem = out.len() % 32;
    if rem != 0 {
        out.extend(std::iter::repeat_n(0u8, 32 - rem));
    }
    out
}

/// `predicate.Bytes()` — recover the raw bytes from the chunked encoding: trim
/// trailing zeros, require the last non-zero byte to be the `0xff` delimiter, and
/// reject excess padding (coreth `predicate.go`). Returns `None` on a malformed
/// encoding.
#[must_use]
pub fn predicate_from_chunks(chunks: &[u8]) -> Option<Vec<u8>> {
    if chunks.is_empty() || !chunks.len().is_multiple_of(32) {
        return None;
    }
    // Trim trailing zeros.
    let mut end = chunks.len();
    while end > 0 && chunks[end - 1] == 0 {
        end -= 1;
    }
    if end == 0 {
        return None; // no delimiter found
    }
    // The number of chunks must match exactly (no excess padding chunks).
    let expected_chunks = end.div_ceil(32);
    if expected_chunks != chunks.len() / 32 {
        return None;
    }
    let delimiter_index = end - 1;
    if chunks[delimiter_index] != PREDICATE_DELIMITER {
        return None;
    }
    Some(chunks[..delimiter_index].to_vec())
}

/// `Signature.NumSigners()` — the popcount of the signer bit-set (spec 20 §7.3).
/// Used by the predicate-gas accounting (`PerWarpSigner * numSigners`). Returns
/// `None` if the signature is not a bit-set signature (only kind in the registry).
#[must_use]
pub fn num_signers(msg: &Message) -> u64 {
    let Signature::BitSet(sig) = &msg.signature;
    Bits::from_bytes(&sig.signers).len()
}

/// `Config.PredicateGas(pred, rules)` — the gas to verify one warp predicate
/// (subnet-evm `warp/config.go`): base + per-chunk + per-signer (spec 20 §7.3).
/// All `SafeMul`/`SafeAdd`. Returns `None` on overflow or a structurally invalid
/// predicate.
#[must_use]
pub fn predicate_gas(chunks: &[u8], is_granite: bool) -> Option<u64> {
    let cfg = current_gas_config(is_granite);
    let num_chunks = (chunks.len() / 32) as u64;
    let bytes_gas = cfg.per_warp_message_chunk.checked_mul(num_chunks)?;
    let mut total = cfg.verify_predicate_base.checked_add(bytes_gas)?;

    let raw = predicate_from_chunks(chunks)?;
    let msg = Message::parse(&raw).ok()?;
    // The payload must parse (coreth validates it before charging signer gas).
    WarpPayload::parse(&msg.unsigned_message.payload).ok()?;

    let signer_gas = num_signers(&msg).checked_mul(cfg.per_warp_signer)?;
    total = total.checked_add(signer_gas)?;
    Some(total)
}

// ---- ABI (hand-rolled, no alloy/ethabi) -----------------------------------

/// ABI-encode a successful `(WarpMessage{bytes32, address, bytes}, bool valid)`
/// return (a dynamic tuple + a bool, so the outer head holds an offset + the
/// bool, and the tuple is appended).
fn pack_verified_message(
    source_chain: &[u8; 32],
    sender: &[u8; 20],
    payload: &[u8],
    valid: bool,
) -> Vec<u8> {
    // Outer head: [0] offset to tuple (0x40), [1] valid.
    let mut out = Vec::new();
    out.extend_from_slice(&u_word(0x40));
    out.extend_from_slice(&bool_word(valid));
    // Tuple: [0] sourceChainID, [1] originSenderAddress, [2] offset to payload (0x60).
    out.extend_from_slice(source_chain);
    out.extend_from_slice(&addr_word(sender));
    out.extend_from_slice(&u_word(0x60));
    // payload: length + padded data.
    out.extend_from_slice(&u_word(payload.len() as u64));
    out.extend_from_slice(payload);
    let pad = (32 - (payload.len() % 32)) % 32;
    out.extend(std::iter::repeat_n(0u8, pad));
    out
}

/// ABI-encode a `(WarpBlockHash{bytes32, bytes32}, bool valid)` return (a static
/// tuple inlined into the head + the bool).
fn pack_verified_block_hash(
    source_chain: &[u8; 32],
    block_hash: &[u8; 32],
    valid: bool,
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(source_chain);
    out.extend_from_slice(block_hash);
    out.extend_from_slice(&bool_word(valid));
    out
}

/// ABI-decode a single `bytes` argument (offset word, length word, data).
fn abi_decode_bytes(input: &[u8]) -> Option<Vec<u8>> {
    if input.len() < 64 {
        return None;
    }
    let off = be_usize(&input[0..32])?;
    if off.checked_add(32)? > input.len() {
        return None;
    }
    let len = be_usize(&input[off..off + 32])?;
    let start = off.checked_add(32)?;
    let end = start.checked_add(len)?;
    if end > input.len() {
        return None;
    }
    Some(input[start..end].to_vec())
}

/// ABI-encode a single `bytes` argument.
fn abi_encode_bytes(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&u_word(0x20));
    out.extend_from_slice(&u_word(b.len() as u64));
    out.extend_from_slice(b);
    let pad = (32 - (b.len() % 32)) % 32;
    out.extend(std::iter::repeat_n(0u8, pad));
    out
}

/// ABI-decode a single `uint32` argument (the value sits in the low 4 bytes of
/// the 32-byte word; coreth uses non-strict unpacking, so high bytes are ignored
/// for the purpose of the conversion but we still require the word to fit u32).
fn abi_decode_u32(input: &[u8]) -> Option<u32> {
    if input.len() < 32 {
        return None;
    }
    // The high 28 bytes must be zero (a valid uint32 padded to 32 bytes).
    if input[0..28].iter().any(|&b| b != 0) {
        return None;
    }
    let mut v = [0u8; 4];
    v.copy_from_slice(&input[28..32]);
    Some(u32::from_be_bytes(v))
}

/// A `uint256` word holding `v`.
fn u_word(v: u64) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[24..32].copy_from_slice(&v.to_be_bytes());
    w
}

/// A `bool` word (`1`/`0` in the low byte).
fn bool_word(v: bool) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[31] = u8::from(v);
    w
}

/// A left-padded `address` word.
fn addr_word(a: &[u8; 20]) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..32].copy_from_slice(a);
    w
}

/// Decode a big-endian `usize` from the low 8 bytes of a 32-byte word; `None` if
/// the high 24 bytes are non-zero (an out-of-range offset/length).
fn be_usize(w: &[u8]) -> Option<usize> {
    if w.len() < 32 || w[0..24].iter().any(|&b| b != 0) {
        return None;
    }
    let mut v = 0usize;
    for &b in &w[24..32] {
        v = (v << 8) | b as usize;
    }
    Some(v)
}

/// Take the rightmost 20 bytes of `b` as an EVM address (a warp source address is
/// the 20-byte caller; longer/shorter sources left-pad/truncate to 20 bytes).
fn left20(b: &[u8]) -> [u8; 20] {
    let mut out = [0u8; 20];
    if b.len() >= 20 {
        out.copy_from_slice(&b[b.len() - 20..]);
    } else {
        out[20 - b.len()..].copy_from_slice(b);
    }
    out
}

// ---- InterpreterResult helpers --------------------------------------------

/// A successful precompile return: `Return`, `output`, with `gas` carrying the
/// already-recorded cost.
fn success(output: Vec<u8>, gas: Gas) -> InterpreterResult {
    InterpreterResult {
        result: InstructionResult::Return,
        output: Bytes::from(output),
        gas,
    }
}

/// A user-triggerable precompile call failure (all supplied gas consumed,
/// geth "precompile returned an error" parity — `gas = 0` in `evm.Call`).
fn failure(gas_limit: u64) -> InterpreterResult {
    let mut g = Gas::new(gas_limit);
    g.spend_all();
    InterpreterResult {
        result: InstructionResult::PrecompileError,
        output: Bytes::new(),
        gas: g,
    }
}

/// An out-of-gas precompile result (all gas consumed).
fn out_of_gas(gas_limit: u64) -> InterpreterResult {
    let mut g = Gas::new(gas_limit);
    g.spend_all();
    InterpreterResult {
        result: InstructionResult::PrecompileOOG,
        output: Bytes::new(),
        gas: g,
    }
}

// `U256` is part of the precompile-ctx value type but unused in the warp bodies
// (warp calls are non-payable); name it to keep the facade import meaningful.
const _: fn(U256) = |_v: U256| {};
