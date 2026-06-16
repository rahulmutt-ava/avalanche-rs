// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! P-Chain genesis state, parse/marshal, the genesis-block derivation, and the
//! `state.State` genesis seeding (`vms/platformvm/genesis/{genesis,codec}.go`
//! + `state.syncGenesis`, specs 23 §3.4/§4.1, 08 §1).
//!
//! This task provides the P-Chain genesis **types** ([`Genesis`], [`GenesisUtxo`],
//! [`Utxo`]) + their byte-exact `GenesisCodec` [`parse`]/[`marshal`], the
//! genesis-**block** derivation ([`genesis_id`]/[`genesis_block`]), and the
//! [`seed_state`] seeding of the M4.13 [`State`](crate::state::state::State). The
//! full byte-exact genesis *construction* pipeline (allocation parsing, the
//! `txheap.ByEndTime` validator ordering, the X/C-Chain `CreateChainTx`
//! assembly — specs 23 §3.1–§3.3) lives in `ava-genesis` (M8) and is out of
//! scope here.
//!
//! ## Wire layout (byte-exact with Go)
//!
//! [`Genesis`] mirrors Go's `genesis.Genesis` field-for-field, in declaration
//! order (= linear-codec order — do **not** reorder). Note the two distinct
//! "message" encodings, faithful to Go:
//! - [`Genesis::message`] is a `String` (Go `Message string` → `u16`-len prefix).
//! - [`GenesisUtxo::message`] is a `Vec<u8>` (Go `UTXO.Message []byte` →
//!   `u32`-len prefix).
//!
//! [`Utxo`] is the codec-serializable mirror of `avax.UTXO`
//! (`UTXOID{tx_id, output_index}` + `Asset{id}` + the fx `Output` interface),
//! analogous to the [`components`](crate::txs::components) byte-exact shapes.

use ava_codec::AvaCodec;
use ava_crypto::hashing;
use ava_types::id::Id;

use crate::CODEC_VERSION;
use crate::block::apricot::{ApricotCommitBlock, CommonBlock};
use crate::block::{Block, BlockBody};
use crate::error::{Error, Result};
use crate::state::chain::Chain;
use crate::state::staker::Staker;
use crate::txs::components::Output;
use crate::txs::{GenesisCodec, Tx, UnsignedTx};

/// `avax.UTXO` — a codec-serializable genesis UTXO (`UTXOID` + `Asset` + fx
/// `Output`).
///
/// Byte-exact mirror of Go `avax.UTXO`: the embedded `UTXOID{TxID, OutputIndex}`
/// then `Asset{ID}` then the `Out` interface (`secp256k1fx.TransferOutput` /
/// `stakeable.LockOut`, carrying its own typeID). The runtime
/// [`ava_vm::components::avax::Utxo`] keeps `Out` as an `Arc<dyn State>` trait
/// object that is not codec-serializable in isolation; this typed shape is what
/// the genesis bytes actually encode (cf. [`crate::txs::components`]).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct Utxo {
    /// `UTXOID.TxID` — the id of the tx that produced this UTXO
    /// ([`Id::EMPTY`] for genesis UTXOs).
    #[codec]
    pub tx_id: Id,
    /// `UTXOID.OutputIndex` — the output index within that tx.
    #[codec]
    pub output_index: u32,
    /// `Asset.ID` — the asset this UTXO is denominated in.
    #[codec]
    pub asset_id: Id,
    /// `Out` — the fx output payload (interface; carries its own typeID).
    #[codec]
    pub out: Output,
}

/// `genesis.UTXO` — an [`avax.UTXO`](Utxo) plus a per-UTXO message.
///
/// The `message` is a `Vec<u8>` (Go `UTXO.Message []byte`, `u32`-len prefix).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct GenesisUtxo {
    /// The embedded `avax.UTXO`.
    #[codec]
    pub utxo: Utxo,
    /// `Message` — arbitrary per-UTXO bytes (`[]byte`).
    #[codec]
    pub message: Vec<u8>,
}

/// `genesis.Genesis` — the genesis state of the Platform Chain (and thereby of
/// the whole Avalanche network), specs 23 §3.4.
///
/// Field order **is** the linear-codec order (do not reorder). Marshalled with
/// the [`GenesisCodec`] (version 0, `i32::MAX` max-slice manager).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct Genesis {
    /// `UTXOs` — the platform-chain UTXOs that exist at genesis.
    #[codec]
    pub utxos: Vec<GenesisUtxo>,
    /// `Validators` — the Primary Network validators at genesis (each an
    /// `AddValidatorTx` / `AddPermissionlessValidatorTx`).
    #[codec]
    pub validators: Vec<Tx>,
    /// `Chains` — the chains that exist at genesis (each a `CreateChainTx`).
    #[codec]
    pub chains: Vec<Tx>,
    /// `Timestamp` — the platform-chain time at network genesis (unix seconds).
    #[codec]
    pub timestamp: u64,
    /// `InitialSupply` — the initial AVAX supply.
    #[codec]
    pub initial_supply: u64,
    /// `Message` — the genesis message (Go `string`, `u16`-len prefix).
    #[codec]
    pub message: String,
}

/// `(*Genesis).Bytes` — marshals the genesis state with the [`GenesisCodec`]
/// (specs 23 §3.4).
///
/// # Errors
/// Returns [`Error::Codec`] if marshalling fails.
pub fn marshal(g: &Genesis) -> Result<Vec<u8>> {
    Ok(GenesisCodec().marshal(CODEC_VERSION, g)?)
}

/// `genesis.Parse` — decodes the genesis state and re-initializes each
/// validator/chain tx's cached `tx_id`/`bytes` with the [`GenesisCodec`]
/// (specs 23 §3.4).
///
/// # Errors
/// Returns [`Error::Codec`] if the bytes fail to decode or a contained tx fails
/// to (re)initialize.
pub fn parse(bytes: &[u8]) -> Result<Genesis> {
    let mut g = Genesis::default();
    GenesisCodec().unmarshal(bytes, &mut g)?;
    for tx in g.validators.iter_mut().chain(g.chains.iter_mut()) {
        tx.initialize(GenesisCodec())?;
    }
    Ok(g)
}

/// `genesis_id = ComputeHash256Array(p_chain_genesis_bytes)` — the hash of the
/// marshalled genesis state bytes (specs 23 §4.1). This is **the** primary
/// golden value, and is the genesis block's *parent*.
#[must_use]
pub fn genesis_id(genesis_bytes: &[u8]) -> Id {
    Id::from(hashing::sha256(genesis_bytes))
}

/// `genesis_block = ApricotCommitBlock { parent_id: genesis_id, height: 0 }`
/// (specs 23 §4.1).
///
/// Returns the initialized [`Block`] (its `block_id`/`bytes` caches populated
/// via the [`GenesisCodec`]). Note the block's *parent* is the hash of the
/// genesis state bytes; the block's own id is `sha256(commit-block bytes)`.
///
/// # Errors
/// Returns [`Error::Codec`] if the block fails to marshal.
pub fn genesis_block(genesis_bytes: &[u8]) -> Result<Block> {
    let parent_id = genesis_id(genesis_bytes);
    let mut block = Block::new(BlockBody::ApricotCommit(ApricotCommitBlock {
        common: CommonBlock {
            parent_id,
            height: 0,
        },
    }));
    block.initialize(GenesisCodec())?;
    Ok(block)
}

/// `state.syncGenesis` — seeds the [`Chain`] write surface from a parsed
/// [`Genesis`] and records the genesis [`ApricotCommitBlock`] as last-accepted
/// **without** calling `Accept()` (specs 23 §4.1).
///
/// Seeds: the chain timestamp (`genesis.timestamp`), the Primary-Network current
/// supply (`genesis.initial_supply`), every genesis UTXO (by its `input_id`),
/// every genesis validator as a *current* Primary-Network validator, and every
/// genesis chain (under its subnet). Unlike Go's `syncGenesis`, the per-validator
/// potential-reward accrual is not applied here (the reward calculator is wired
/// by a later task); the validators are seeded with `potential_reward = 0` and
/// the supply is left at `initial_supply` exactly as the task specifies.
///
/// The genesis block ID is returned so the caller can record it as the VM's
/// last-accepted block.
///
/// # Errors
/// Returns the matching [`Error`] for a malformed validator tx (a non-staker
/// `UnsignedTx`, an absent BLS key on a PoP signer, an over-`u64` supply add),
/// or a codec failure deriving the genesis block.
pub fn seed_state<C: Chain>(state: &mut C, genesis: &Genesis, genesis_bytes: &[u8]) -> Result<Id> {
    use std::time::{Duration, UNIX_EPOCH};

    let block = genesis_block(genesis_bytes)?;
    let genesis_blk_id = block.id();

    // Chain time + Primary-Network current supply.
    let timestamp = UNIX_EPOCH
        .checked_add(Duration::from_secs(genesis.timestamp))
        .ok_or(Error::Overflow)?;
    state.set_timestamp(timestamp);
    state.set_current_supply(Id::EMPTY, genesis.initial_supply);

    // Genesis UTXOs, keyed by their input id (`tx_id.prefix(output_index)`).
    for gutxo in &genesis.utxos {
        let utxo_id = gutxo
            .utxo
            .tx_id
            .prefix(&[u64::from(gutxo.utxo.output_index)]);
        let utxo_bytes = GenesisCodec().marshal(CODEC_VERSION, &gutxo.utxo)?;
        state.add_utxo(utxo_id, utxo_bytes);
    }

    // Primary-Network validators (each a current validator). The tx bytes are
    // stored in the tx store (Go `state.AddTx`) so the reward-proposal executor's
    // `staker_tx_resolver` (Go `state.GetTx`) can recover and reward a genesis
    // validator — without this a genesis validator is never rewardable
    // (M4.24 / M9.19 Gap 2).
    for vdr_tx in &genesis.validators {
        let staker = staker_from_validator_tx(vdr_tx)?;
        state.put_current_validator(staker)?;
        state.add_tx(vdr_tx.id(), vdr_tx.bytes().to_vec());
    }

    // Genesis chains (recorded under their subnet).
    for chain_tx in &genesis.chains {
        match &chain_tx.unsigned {
            UnsignedTx::CreateChain(c) => {
                state.add_chain(c.subnet_id, chain_tx.id());
            }
            _ => return Err(Error::WrongTxType),
        }
    }

    Ok(genesis_blk_id)
}

/// Builds the *current* [`Staker`] for a genesis validator tx (an
/// `AddValidatorTx` or `AddPermissionlessValidatorTx`); any other tx type is an
/// [`Error::WrongTxType`].
///
/// The BLS public key (Primary-Network PoP signer) is recovered from the
/// `AddPermissionlessValidatorTx.signer`; an `AddValidatorTx` has none.
fn staker_from_validator_tx(tx: &Tx) -> Result<Staker> {
    use std::time::{Duration, UNIX_EPOCH};

    let (validator, public_key, subnet) = match &tx.unsigned {
        UnsignedTx::AddValidator(v) => (&v.validator, None, Id::EMPTY),
        UnsignedTx::AddPermissionlessValidator(v) => (&v.validator, v.signer.key()?, v.subnet),
        _ => return Err(Error::WrongTxType),
    };

    let start_time = UNIX_EPOCH
        .checked_add(Duration::from_secs(validator.start))
        .ok_or(Error::Overflow)?;
    let end_time = UNIX_EPOCH
        .checked_add(Duration::from_secs(validator.end))
        .ok_or(Error::Overflow)?;

    Ok(Staker::new_current(
        tx.id(),
        validator.node_id,
        public_key,
        subnet,
        validator.wght,
        start_time,
        end_time,
        // Potential reward accrual is applied by the reward-wired acceptor; the
        // genesis seeding leaves it at 0 (task M4.24 §Implementation).
        0,
        crate::txs::Priority::PrimaryNetworkValidatorCurrent,
    ))
}

#[cfg(test)]
pub(crate) use test_support::test_synthetic_genesis;

#[cfg(test)]
mod test_support {
    use ava_secp256k1fx::{OutputOwners, TransferInput, TransferOutput};
    use ava_types::id::Id;
    use ava_types::node_id::NodeId;
    use ava_types::short_id::ShortId;

    use super::{Genesis, GenesisUtxo, Utxo};
    use crate::signer::{ProofOfPossession, Signer};
    use crate::txs::base_tx::BaseTx;
    use crate::txs::components::{
        BaseTx as AvaxBaseTx, Input, Output, Owner, TransferableInput, TransferableOutput,
    };
    use crate::txs::create_chain::CreateChainTx;
    use crate::txs::validator::Validator;
    use crate::txs::{AddPermissionlessValidatorTx, GenesisCodec, Tx, UnsignedTx};

    /// The Mainnet AVAX asset id used throughout the Go serialization vectors.
    const AVAX_ASSET_ID: [u8; 32] = [
        0x21, 0xe6, 0x73, 0x17, 0xcb, 0xc4, 0xbe, 0x2a, 0xeb, 0x00, 0x67, 0x7a, 0xd6, 0x46, 0x27,
        0x78, 0xa8, 0xf5, 0x22, 0x74, 0xb9, 0xd6, 0x05, 0xdf, 0x25, 0x91, 0xb2, 0x30, 0x27, 0xa8,
        0x7d, 0xff,
    ];
    const ADDR: [u8; 20] = [
        0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa,
        0xbb, 0x44, 0x55, 0x66, 0x77,
    ];
    /// The BLS compressed public key from the Go vectors.
    const BLS_PUBKEY: [u8; 48] = [
        0xaf, 0xf4, 0xac, 0xb4, 0xc5, 0x43, 0x9b, 0x5d, 0x42, 0x6c, 0xad, 0xf9, 0xe9, 0x46, 0xd3,
        0xa4, 0x52, 0xf7, 0xde, 0x34, 0x14, 0xd1, 0xad, 0x27, 0x33, 0x61, 0x33, 0x21, 0x1d, 0x8b,
        0x90, 0xcf, 0x49, 0xfb, 0x97, 0xee, 0xbc, 0xde, 0xee, 0xf7, 0x14, 0xdc, 0x20, 0xf5, 0x4e,
        0xd0, 0xd4, 0xd1,
    ];
    /// The BLS proof-of-possession signature from the Go vectors.
    const BLS_SIG: [u8; 96] = [
        0x8c, 0xfd, 0x79, 0x09, 0xd1, 0x53, 0xb9, 0x60, 0x4b, 0x62, 0xb1, 0x43, 0xba, 0x36, 0x20,
        0x7b, 0xb7, 0xe6, 0x48, 0x67, 0x42, 0x44, 0x80, 0x20, 0x2a, 0x67, 0xdc, 0x68, 0x76, 0x83,
        0x46, 0xd9, 0x5c, 0x90, 0x98, 0x3c, 0x2d, 0x27, 0x9c, 0x64, 0xc4, 0x3c, 0x51, 0x13, 0x6b,
        0x2a, 0x05, 0xe0, 0x16, 0x02, 0xd5, 0x2a, 0xa6, 0x37, 0x6f, 0xda, 0x17, 0xfa, 0x6e, 0x2a,
        0x18, 0xa0, 0x83, 0xe4, 0x9d, 0x9c, 0x45, 0x0e, 0xab, 0x7b, 0x89, 0xb1, 0xd5, 0x55, 0x5d,
        0xa5, 0xc4, 0x89, 0x87, 0x2e, 0x02, 0xb7, 0xe5, 0x22, 0x7b, 0x77, 0x55, 0x0a, 0xf1, 0x33,
        0x0e, 0x5a, 0x71, 0xf8, 0xc3, 0x68,
    ];

    fn owners_one_addr() -> OutputOwners {
        OutputOwners::new(0, 1, vec![ShortId::from(ADDR)])
    }

    /// A deterministic synthetic [`Genesis`] — two genesis UTXOs, one Primary
    /// Network permissionless validator, and one `CreateChainTx` — built
    /// directly from the typed components (no `ava-genesis` M8 construction
    /// pipeline). Used to exercise the derivation invariants + parse/marshal
    /// round-trip until the real Fuji bytes are available (see the deferred
    /// `Fuji p_chain_genesis_bytes` row in `tests/PORTING.md`).
    pub fn test_synthetic_genesis() -> Genesis {
        const KILO_AVAX: u64 = 2_000 * 1_000_000_000;
        let avax = Id::from(AVAX_ASSET_ID);

        let utxo0 = GenesisUtxo {
            utxo: Utxo {
                tx_id: Id::EMPTY,
                output_index: 0,
                asset_id: avax,
                out: Output::Transfer(TransferOutput::new(123_456_789, owners_one_addr())),
            },
            message: vec![],
        };
        let utxo1 = GenesisUtxo {
            utxo: Utxo {
                tx_id: Id::EMPTY,
                output_index: 1,
                asset_id: avax,
                out: Output::Transfer(TransferOutput::new(987_654_321, owners_one_addr())),
            },
            message: b"hi".to_vec(),
        };

        let mut vdr = Tx::new(UnsignedTx::AddPermissionlessValidator(
            AddPermissionlessValidatorTx {
                base: BaseTx::new(AvaxBaseTx {
                    network_id: 1,
                    blockchain_id: Id::EMPTY,
                    outs: vec![],
                    ins: vec![TransferableInput {
                        tx_id: Id::EMPTY,
                        output_index: 0,
                        asset_id: avax,
                        r#in: Input::Transfer(TransferInput::new(KILO_AVAX, vec![0])),
                    }],
                    memo: vec![],
                }),
                validator: Validator {
                    node_id: NodeId::from([
                        0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x11, 0x22, 0x33, 0x44,
                        0x55, 0x66, 0x77, 0x88, 0x11, 0x22, 0x33, 0x44,
                    ]),
                    start: 5,
                    end: 5 + 365 * 24 * 60 * 60,
                    wght: KILO_AVAX,
                },
                subnet: Id::EMPTY,
                signer: Signer::ProofOfPossession(ProofOfPossession::new(BLS_PUBKEY, BLS_SIG)),
                stake_outs: vec![TransferableOutput {
                    asset_id: avax,
                    out: Output::Transfer(TransferOutput::new(KILO_AVAX, owners_one_addr())),
                }],
                validator_rewards_owner: Owner::Secp256k1(owners_one_addr()),
                delegator_rewards_owner: Owner::Secp256k1(owners_one_addr()),
                delegation_shares: 1_000_000,
                verified: std::cell::OnceCell::new(),
            },
        ));
        vdr.initialize(GenesisCodec()).expect("init validator tx");

        let mut chain = Tx::new(UnsignedTx::CreateChain(CreateChainTx {
            base: BaseTx::new(AvaxBaseTx {
                network_id: 1,
                blockchain_id: Id::EMPTY,
                outs: vec![],
                ins: vec![],
                memo: vec![],
            }),
            subnet_id: Id::EMPTY,
            chain_name: "X-Chain".to_string(),
            vm_id: Id::from([0x07; 32]),
            fx_ids: vec![Id::from([0x09; 32])],
            genesis_data: b"avm-genesis".to_vec(),
            subnet_auth: crate::txs::components::Auth::default(),
        }));
        chain.initialize(GenesisCodec()).expect("init chain tx");

        Genesis {
            utxos: vec![utxo0, utxo1],
            validators: vec![vdr],
            chains: vec![chain],
            timestamp: 5,
            initial_supply: 360_000_000 * 1_000_000_000,
            message: "synthetic genesis".to_string(),
        }
    }
}

#[cfg(test)]
mod golden {
    use ava_crypto::hashing;
    use ava_types::id::Id;

    use crate::genesis::{self, Genesis};

    /// TDD ENTRY POINT (M4.24): `golden::pchain_genesis_block_id`.
    #[test]
    fn pchain_genesis_block_id() {
        let g: Genesis = genesis::test_synthetic_genesis();
        let bytes = genesis::marshal(&g).expect("marshal genesis");

        // genesis_id == sha256(genesis_bytes).
        let genesis_id = genesis::genesis_id(&bytes);
        assert_eq!(genesis_id, Id::from(hashing::sha256(&bytes)));

        // genesis_block = ApricotCommitBlock { parent_id: genesis_id, height: 0 }.
        let block = genesis::genesis_block(&bytes).expect("derive genesis block");
        assert_eq!(block.parent_id(), genesis_id);
        assert_eq!(block.height(), 0);

        // round-trip: parse(marshal(g)) == g.
        let parsed = genesis::parse(&bytes).expect("parse genesis");
        assert_eq!(parsed, g);
    }
}

#[cfg(test)]
mod seed {
    use ava_database::MemDb;
    use ava_types::id::Id;

    use crate::genesis;
    use crate::state::chain::Chain;
    use crate::state::state::State;

    /// Seeding the M4.13 `State` from genesis records the timestamp, supply,
    /// UTXOs, current validators, and chains, and returns the genesis block id.
    #[test]
    fn seed_state_records_genesis() {
        let g = genesis::test_synthetic_genesis();
        let bytes = genesis::marshal(&g).expect("marshal");
        let mut state = State::new(MemDb::new()).expect("state");

        let blk_id = genesis::seed_state(&mut state, &g, &bytes).expect("seed");

        // The returned id is the genesis ApricotCommit block's id.
        let expected = genesis::genesis_block(&bytes).expect("block").id();
        assert_eq!(blk_id, expected);

        // Primary-Network supply == initial_supply.
        assert_eq!(
            state.current_supply(Id::EMPTY).expect("supply"),
            g.initial_supply
        );

        // Both genesis UTXOs are present (keyed by input id).
        for gutxo in &g.utxos {
            let id = gutxo
                .utxo
                .tx_id
                .prefix(&[u64::from(gutxo.utxo.output_index)]);
            assert!(state.get_utxo(id).is_ok(), "genesis UTXO missing");
        }

        // The single genesis validator is a current Primary-Network validator.
        assert_eq!(state.current_stakers().len(), 1);

        // The genesis chain is recorded under the Primary Network subnet.
        assert_eq!(state.chains(Id::EMPTY).len(), 1);
    }

    /// Regression (M4.24 / M9.19 Gap 2): seeding records each genesis validator's
    /// tx **bytes** in the state tx store, so the reward-proposal executor's
    /// `staker_tx_resolver` (Go `state.GetTx`) can recover and reward a genesis
    /// validator. Before this fix `get_tx(vdr.id())` returned `NotFound`, leaving
    /// genesis validators permanently unrewardable.
    #[test]
    fn seed_state_records_genesis_validator_tx() {
        use crate::txs::{Codec, Tx};

        let g = genesis::test_synthetic_genesis();
        let bytes = genesis::marshal(&g).expect("marshal");
        let mut state = State::new(MemDb::new()).expect("state");

        genesis::seed_state(&mut state, &g, &bytes).expect("seed");

        let vdr = g.validators.first().expect("one genesis validator");
        assert!(
            !vdr.bytes().is_empty(),
            "genesis validator tx bytes populated"
        );

        // The validator tx bytes are stored under its id and match exactly.
        let stored = state
            .get_tx(vdr.id())
            .expect("genesis validator tx in store");
        assert_eq!(stored, vdr.bytes(), "stored bytes == validator tx bytes");

        // Those stored bytes resolve through the proposal executor's path:
        // parse with the regular codec, then project to a RewardedStakerTx.
        let parsed = Tx::parse(Codec(), &stored).expect("parse stored validator tx");
        assert!(
            crate::block::executor::verify::rewarded_staker_tx(&parsed).is_some(),
            "genesis validator resolves to a rewardable staker tx"
        );
    }
}
