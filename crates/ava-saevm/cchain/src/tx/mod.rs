// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! C-Chain atomic Import/Export transaction model (specs/11 §8, 27 §3.1).
//!
//! Port of `vms/saevm/cchain/tx`. A signed [`Tx`] wraps an [`Unsigned`] body
//! ([`Import`] or [`Export`]) and its fx [`Credential`]s. The bytes are the
//! avalanchego linear codec (`03`), with typeIDs aligned to the X-Chain/P-Chain
//! so UTXOs in shared memory share one serialized format (Go
//! `cchain/tx/codec.go` skips registrations: `Import`=0, `Export`=1,
//! `secp256k1fx.TransferInput`=5, `secp256k1fx.TransferOutput`=7,
//! `secp256k1fx.Credential`=9).
//!
//! * an [`Import`] consumes UTXOs in shared memory (`imported_ins`) and credits
//!   EVM accounts ([`Output`]s) — it **mints** AVAX into C-Chain state;
//! * an [`Export`] debits EVM accounts ([`Input`]s, an account+nonce pair) and
//!   produces UTXOs in shared memory (`exported_outs`) — it **burns** AVAX.
//!
//! [`Tx::as_op`] maps the tx onto the SAE [`hook::Op`] applied during block
//! execution (the seam M7.21's `AtomicOp` describes); [`Tx::atomic_requests`]
//! returns the shared-memory mutation merged into the accept batch (27 §2.3).

pub mod components;

use ava_codec::AvaCodec;
use ava_codec::manager::Manager;
use ava_crypto::hashing;
use ava_saevm_hook::op::{AccountDebit, Op};
use ava_saevm_types::{Address, U256};
use ava_secp256k1fx::Credential as SecpCredential;
use ava_types::id::Id;
use ava_vm::components::avax::shared_memory::{Element, Requests};
use ava_vm::components::gas::Gas;

use crate::tx::components::{Output as FxOutput, TransferableInput, TransferableOutput, Utxo};

/// The C-Chain atomic-tx codec version (`codecVersion = 0`).
pub const CODEC_VERSION: u16 = 0;

/// The conversion rate between 1 nAVAX (X/P-Chain denomination) and 1 aAVAX
/// (C-Chain/EVM denomination) — `x2cRate = 1e9` (Go `tx._x2cRate`).
pub const X2C_RATE: u64 = 1_000_000_000;

/// `tx.ScaleAVAX` — scale a nAVAX amount up to the C-Chain's aAVAX denomination.
#[must_use]
pub fn scale_avax(v: u64) -> U256 {
    U256::from(v).saturating_mul(U256::from(X2C_RATE))
}

/// Errors returned by the C-Chain atomic tx layer.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A tx failed to (un)marshal through the linear codec.
    #[error("codec: {0}")]
    Codec(#[from] ava_codec::error::CodecError),
    /// An amount overflowed `U256` while building the [`Op`].
    #[error("amount overflow for address {0:#x}")]
    Overflow(Address),
    /// An export referenced one address with two different nonces.
    #[error("multiple nonces for address {0:#x}")]
    MultipleNonces(Address),
}

/// Converts a 20-byte EVM address into a reth [`Address`].
fn to_address(bytes: [u8; 20]) -> Address {
    Address::new(bytes)
}

/// `tx.Output` — an account on the C-Chain whose balance of `asset_id` should be
/// increased by `amount` (scaled up if `asset_id` is AVAX). Used by [`Import`].
#[derive(AvaCodec, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Output {
    /// The EVM account credited.
    #[codec]
    pub address: [u8; 20],
    /// The amount of `asset_id` credited (nAVAX; scaled to aAVAX for AVAX).
    #[codec]
    pub amount: u64,
    /// The asset credited.
    #[codec]
    pub asset_id: Id,
}

/// `tx.Input` — an account+nonce pair on the C-Chain authorizing a debit of
/// `amount` of `asset_id` (scaled up if AVAX). Used by [`Export`].
#[derive(AvaCodec, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Input {
    /// The EVM account debited.
    #[codec]
    pub address: [u8; 20],
    /// The amount of `asset_id` debited (nAVAX; scaled to aAVAX for AVAX).
    #[codec]
    pub amount: u64,
    /// The asset debited.
    #[codec]
    pub asset_id: Id,
    /// The nonce authorizing the debit.
    #[codec]
    pub nonce: u64,
}

/// `tx.Import` — transfers assets from the P/X-Chain to the C-Chain. Consumes
/// UTXOs in shared memory and increases C-Chain balances.
///
/// Wire layout: `network_id u32 | blockchain_id [32] | source_chain [32] |
/// imported_ins []TransferableInput | outs []Output`.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct Import {
    /// The network this chain lives on.
    #[codec]
    pub network_id: u32,
    /// The C-Chain id (prevents cross-chain replay).
    #[codec]
    pub blockchain_id: Id,
    /// The chain the imported UTXOs originate from.
    #[codec]
    pub source_chain: Id,
    /// The inputs spending imported UTXOs (from shared memory).
    #[codec]
    pub imported_ins: Vec<TransferableInput>,
    /// The EVM accounts credited the imported assets.
    #[codec]
    pub outs: Vec<Output>,
}

/// `tx.Export` — transfers assets from the C-Chain to the P/X-Chain. Debits
/// C-Chain balances and produces UTXOs in shared memory.
///
/// Wire layout: `network_id u32 | blockchain_id [32] | destination_chain [32] |
/// ins []Input | exported_outs []TransferableOutput`.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct Export {
    /// The network this chain lives on.
    #[codec]
    pub network_id: u32,
    /// The C-Chain id (prevents cross-chain replay).
    #[codec]
    pub blockchain_id: Id,
    /// The chain the exported UTXOs are destined for.
    #[codec]
    pub destination_chain: Id,
    /// The C-Chain accounts debited.
    #[codec]
    pub ins: Vec<Input>,
    /// The outputs sent to the destination chain (produced in shared memory).
    #[codec]
    pub exported_outs: Vec<TransferableOutput>,
}

/// `tx.Unsigned` — the interface body of an atomic [`Tx`], `Import`=0/`Export`=1.
#[derive(AvaCodec, Clone, Debug, PartialEq, Eq)]
#[codec(type_registry)]
pub enum Unsigned {
    /// `Import` (`type_id` 0).
    #[codec(type_id = 0)]
    Import(Import),
    /// `Export` (`type_id` 1).
    #[codec(type_id = 1)]
    Export(Export),
}

impl Default for Unsigned {
    fn default() -> Self {
        Unsigned::Import(Import::default())
    }
}

/// `tx.Credential` — the registered credential interface (`secp256k1fx`=9).
#[derive(AvaCodec, Clone, Debug, PartialEq, Eq)]
#[codec(type_registry)]
pub enum Credential {
    /// `secp256k1fx.Credential` (`type_id` 9).
    #[codec(type_id = 9)]
    Secp256k1(SecpCredential),
}

impl Default for Credential {
    fn default() -> Self {
        Credential::Secp256k1(SecpCredential::default())
    }
}

/// `tx.Tx` — a signed atomic transaction.
///
/// The `unsigned` body and `creds` are serialized (in that order); the id is
/// derived (`sha256(signed_bytes)`) and is not on the wire.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct Tx {
    /// The transaction body (interface → typeid-prefixed).
    #[codec]
    pub unsigned: Unsigned,
    /// The fx credentials (each interface → typeid-prefixed).
    #[codec]
    pub creds: Vec<Credential>,
}

impl Tx {
    /// `Tx.Bytes` — the canonical binary format of the transaction.
    ///
    /// # Errors
    /// Returns [`Error::Codec`] if marshalling fails.
    pub fn marshal(&self) -> Result<Vec<u8>, Error> {
        Ok(codec().marshal(CODEC_VERSION, self)?)
    }

    /// `tx.Parse` — deserialize a [`Tx`] from its canonical binary format.
    ///
    /// # Errors
    /// Returns [`Error::Codec`] if the bytes fail to decode.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let mut tx = Tx::default();
        codec().unmarshal(bytes, &mut tx)?;
        Ok(tx)
    }

    /// `Tx.ID` — `sha256(signed_bytes)`. Returns [`Id::EMPTY`] for an
    /// un-marshalable (invalid) tx, mirroring Go's zero-ID fallback.
    #[must_use]
    pub fn id(&self) -> Id {
        match self.marshal() {
            Ok(bytes) => Id::from(hashing::sha256(&bytes)),
            Err(_) => Id::EMPTY,
        }
    }

    /// The set of one-time-use input ids this tx consumes (`Tx.InputIDs`).
    ///
    /// [`Import`] returns consumed UTXO ids; [`Export`] returns account+nonce
    /// ids.
    #[must_use]
    pub fn input_ids(&self) -> Vec<Id> {
        match &self.unsigned {
            Unsigned::Import(i) => i
                .imported_ins
                .iter()
                .map(TransferableInput::input_id)
                .collect(),
            Unsigned::Export(e) => e
                .ins
                .iter()
                .map(|in_| account_input_id(in_.address, in_.nonce))
                .collect(),
        }
    }

    /// `Tx.AsOp` — convert the tx into the SAE [`Op`] applied during execution
    /// (AVAX-native state changes only; no shared-memory or non-AVAX changes).
    ///
    /// # Errors
    /// Returns [`Error::Overflow`] / [`Error::MultipleNonces`] on a malformed
    /// op.
    pub fn as_op(&self, avax_asset_id: Id) -> Result<Op, Error> {
        let id = self.id();
        match &self.unsigned {
            Unsigned::Import(i) => {
                let mut mint: std::collections::BTreeMap<Address, U256> =
                    std::collections::BTreeMap::new();
                for out in &i.outs {
                    if out.asset_id != avax_asset_id {
                        continue;
                    }
                    let addr = to_address(out.address);
                    let amount = scale_avax(out.amount);
                    let entry = mint.entry(addr).or_insert(U256::ZERO);
                    *entry = entry.checked_add(amount).ok_or(Error::Overflow(addr))?;
                }
                Ok(Op {
                    id,
                    gas: Gas(0),
                    gas_fee_cap: U256::ZERO,
                    burn: std::collections::BTreeMap::new(),
                    mint,
                })
            }
            Unsigned::Export(e) => {
                let mut burn: std::collections::BTreeMap<Address, AccountDebit> =
                    std::collections::BTreeMap::new();
                for in_ in &e.ins {
                    let addr = to_address(in_.address);
                    let debit = burn.entry(addr).or_insert(AccountDebit {
                        nonce: in_.nonce,
                        amount: U256::ZERO,
                        min_balance: U256::ZERO,
                    });
                    if debit.nonce != in_.nonce {
                        return Err(Error::MultipleNonces(addr));
                    }
                    if in_.asset_id == avax_asset_id {
                        let amount = scale_avax(in_.amount);
                        debit.amount = debit
                            .amount
                            .checked_add(amount)
                            .ok_or(Error::Overflow(addr))?;
                    }
                    debit.nonce = in_.nonce;
                    debit.min_balance = debit.amount;
                }
                Ok(Op {
                    id,
                    gas: Gas(0),
                    gas_fee_cap: U256::ZERO,
                    burn,
                    mint: std::collections::BTreeMap::new(),
                })
            }
        }
    }

    /// `Tx.AtomicRequests` — the shared-memory mutation this tx applies on the
    /// peer chain during execution (merged into the accept batch, 27 §2.3).
    ///
    /// [`Import`] removes the consumed UTXOs from the source chain;
    /// [`Export`] puts the produced UTXOs into the destination chain.
    ///
    /// # Errors
    /// Returns [`Error::Codec`] if a produced UTXO fails to marshal.
    pub fn atomic_requests(&self) -> Result<(Id, Requests), Error> {
        let tx_id = self.id();
        match &self.unsigned {
            Unsigned::Import(i) => {
                let remove: Vec<Vec<u8>> = i
                    .imported_ins
                    .iter()
                    .map(|in_| in_.input_id().to_bytes().to_vec())
                    .collect();
                Ok((
                    i.source_chain,
                    Requests {
                        remove,
                        put: Vec::new(),
                    },
                ))
            }
            Unsigned::Export(e) => {
                let mut put = Vec::with_capacity(e.exported_outs.len());
                for (index, out) in e.exported_outs.iter().enumerate() {
                    let output_index = u32::try_from(index).unwrap_or(u32::MAX);
                    let utxo = Utxo {
                        tx_id,
                        output_index,
                        asset_id: out.asset_id,
                        out: out.out.clone(),
                    };
                    let utxo_bytes = codec().marshal(CODEC_VERSION, &utxo)?;
                    let key = utxo.input_id().to_bytes().to_vec();
                    let traits = output_traits(&out.out);
                    put.push(Element {
                        key,
                        value: utxo_bytes,
                        traits,
                    });
                }
                Ok((
                    e.destination_chain,
                    Requests {
                        remove: Vec::new(),
                        put,
                    },
                ))
            }
        }
    }
}

/// `tx.AccountInputID` — the account+nonce pair as a unique [`Id`]:
/// `nonce (8 BE bytes) ‖ address (last 24 of the 32-byte id)`.
#[must_use]
pub fn account_input_id(address: [u8; 20], nonce: u64) -> Id {
    let mut id = [0u8; 32];
    id[0..8].copy_from_slice(&nonce.to_be_bytes());
    id[8..28].copy_from_slice(&address);
    Id::from(id)
}

/// The indexable traits (owner addresses) of a produced UTXO output.
fn output_traits(out: &FxOutput) -> Vec<Vec<u8>> {
    match out {
        FxOutput::SecpTransfer(o) => o
            .owners
            .addrs
            .iter()
            .map(|a| a.as_bytes().to_vec())
            .collect(),
    }
}

/// The process-wide C-Chain atomic-tx codec manager.
///
/// # Panics
/// Never: a fresh default manager cannot fail to register the linear codec.
#[must_use]
pub fn codec() -> &'static Manager {
    use std::sync::OnceLock;

    use ava_codec::linearcodec::LinearCodec;

    static CODEC: OnceLock<Manager> = OnceLock::new();
    CODEC.get_or_init(|| {
        let m = Manager::with_default_max_size();
        let _ = m.register(CODEC_VERSION, std::sync::Arc::new(LinearCodec::new()));
        m
    })
}

/// `tx.MarshalSlice` — the canonical bytes of a slice of txs (nil for empty).
///
/// # Errors
/// Returns [`Error::Codec`] if marshalling fails.
pub fn marshal_slice(txs: &[Tx]) -> Result<Vec<u8>, Error> {
    if txs.is_empty() {
        return Ok(Vec::new());
    }
    let owned: Vec<Tx> = txs.to_vec();
    Ok(codec().marshal(CODEC_VERSION, &owned)?)
}

/// `tx.ParseSlice` — decode a slice of txs (empty input → empty slice).
///
/// # Errors
/// Returns [`Error::Codec`] if the bytes fail to decode.
pub fn parse_slice(bytes: &[u8]) -> Result<Vec<Tx>, Error> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let mut txs: Vec<Tx> = Vec::new();
    codec().unmarshal(bytes, &mut txs)?;
    Ok(txs)
}
