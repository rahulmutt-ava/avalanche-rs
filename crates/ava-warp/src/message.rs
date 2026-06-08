// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The ACP-77 Warp registry payloads (`vms/platformvm/warp/message/**`, specs 20
//! §3.1).
//!
//! This is the **third** of the three nested Warp codecs (specs 20 §3.1): the
//! [`AddressedCall.payload`](crate::payload::AddressedCall::payload) bytes decode
//! to a [`RegistryPayload`]. The P-Chain L1 lifecycle (`08` §6) consumes
//! [`RegisterL1Validator`] and [`L1ValidatorWeight`].
//!
//! > **Module naming.** specs 20 §1 names this module `registry`; this crate
//! > keeps the original P-Chain name `message` (its type registry and wire
//! > layout are identical) to minimise re-pointing churn.
//!
//! Registration order (= type IDs), mirroring Go `warp/message/codec.go`:
//!
//! | Registry payload | Type ID |
//! |---|---|
//! | `SubnetToL1Conversion`    | **0** |
//! | `RegisterL1Validator`     | **1** |
//! | `L1ValidatorRegistration` | **2** |
//! | `L1ValidatorWeight`       | **3** |

use std::sync::{Arc, OnceLock};

use ava_codec::AvaCodec;
use ava_codec::error::Result as CodecResult;
use ava_codec::linearcodec::LinearCodec;
use ava_codec::manager::Manager;
use ava_crypto::bls;
use ava_crypto::hashing;
use ava_types::id::Id;
use ava_types::short_id::{SHORT_ID_LEN, ShortId};

use crate::CODEC_VERSION;
use crate::error::{Error, Result};

/// `message.PChainOwner` — a threshold + addresses owner embedded in the ACP-77
/// registry payloads (`warp/message/register_l1_validator.go`, specs 20 §3.1).
///
/// Distinct from a P-Chain tx-component owner only by codec home (this one lives
/// in the warp-message registry); the wire layout is identical.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct PChainOwner {
    /// Threshold number of `addresses` that must sign.
    #[codec]
    pub threshold: u32,
    /// The addresses allowed to sign to authenticate this owner.
    #[codec]
    pub addresses: Vec<ShortId>,
}

impl PChainOwner {
    /// `verify.All(OutputOwners)` — an owner is valid iff it is either empty
    /// (threshold 0, no addresses) or has `0 < threshold <= len(addresses)`
    /// (`secp256k1fx.OutputOwners.Verify`).
    #[must_use]
    pub fn is_valid(&self) -> bool {
        if self.threshold == 0 {
            return self.addresses.is_empty();
        }
        (self.threshold as usize) <= self.addresses.len()
    }
}

/// `message.RegisterL1Validator` — adds a validator to a subnet
/// (`warp/message/register_l1_validator.go`).
#[derive(AvaCodec, Clone, Debug, PartialEq, Eq)]
pub struct RegisterL1Validator {
    /// `SubnetID` — the subnet the validator is being added to.
    #[codec]
    pub subnet_id: Id,
    /// `NodeID` — the validating node, length-prefixed raw bytes (Go
    /// `JSONByteSlice`).
    #[codec]
    pub node_id: Vec<u8>,
    /// `BLSPublicKey` — the validator's compressed (48-byte) BLS public key.
    #[codec]
    pub bls_public_key: [u8; bls::PUBLIC_KEY_LEN],
    /// `Expiry` — the Unix timestamp (seconds) after which this message is no
    /// longer valid.
    #[codec]
    pub expiry: u64,
    /// `RemainingBalanceOwner` — owner of leftover $AVAX on removal.
    #[codec]
    pub remaining_balance_owner: PChainOwner,
    /// `DisableOwner` — owner with authority to disable the validator.
    #[codec]
    pub disable_owner: PChainOwner,
    /// `Weight` — the validator's sampling weight.
    #[codec]
    pub weight: u64,
}

impl Default for RegisterL1Validator {
    fn default() -> Self {
        Self {
            subnet_id: Id::EMPTY,
            node_id: Vec::new(),
            bls_public_key: [0u8; bls::PUBLIC_KEY_LEN],
            expiry: 0,
            remaining_balance_owner: PChainOwner::default(),
            disable_owner: PChainOwner::default(),
            weight: 0,
        }
    }
}

impl RegisterL1Validator {
    /// `RegisterL1Validator.Verify()` — the structural checks (`subnetID` is not
    /// the Primary Network, non-zero weight, a valid non-empty node id, and valid
    /// owners).
    ///
    /// # Errors
    /// Returns [`Error::InvalidPayload`] if any structural check fails (Go's
    /// `ErrInvalidSubnetID`/`ErrInvalidWeight`/`ErrInvalidNodeID`/`ErrInvalidOwner`
    /// all map to the single component-invalid sentinel here).
    pub fn verify(&self) -> Result<()> {
        if self.subnet_id == Id::EMPTY {
            // PrimaryNetworkID is the empty id.
            return Err(Error::InvalidPayload);
        }
        if self.weight == 0 {
            return Err(Error::InvalidPayload);
        }
        // `ids.ToNodeID` requires exactly 20 bytes and a non-empty node id.
        if self.node_id.len() != SHORT_ID_LEN || self.node_id.iter().all(|&b| b == 0) {
            return Err(Error::InvalidPayload);
        }
        if !self.remaining_balance_owner.is_valid() || !self.disable_owner.is_valid() {
            return Err(Error::InvalidPayload);
        }
        Ok(())
    }

    /// `RegisterL1Validator.ValidationID()` — `sha256` over the marshaled
    /// registry-payload bytes (Go `hashing.ComputeHash256Array(r.Bytes())`).
    ///
    /// `bytes` must be the exact [`RegistryPayload`] wire bytes this payload was
    /// parsed from (the AddressedCall inner payload), so the hash matches Go.
    #[must_use]
    pub fn validation_id(bytes: &[u8]) -> Id {
        Id::from(hashing::sha256(bytes))
    }
}

/// `message.L1ValidatorWeight` — sets an L1 validator's weight
/// (`warp/message/l1_validator_weight.go`).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct L1ValidatorWeight {
    /// `ValidationID` — the validator to update.
    #[codec]
    pub validation_id: Id,
    /// `Nonce` — the monotonic update nonce (`MaxUint64` reserved for removal).
    #[codec]
    pub nonce: u64,
    /// `Weight` — the new weight (`0` removes the validator).
    #[codec]
    pub weight: u64,
}

impl L1ValidatorWeight {
    /// `L1ValidatorWeight.Verify()` — `MaxUint64` nonce is reserved for removal,
    /// so it is only valid with a zero weight.
    ///
    /// # Errors
    /// Returns [`Error::InvalidPayload`] (Go `ErrNonceReservedForRemoval`) when
    /// `nonce == u64::MAX && weight != 0`.
    pub fn verify(&self) -> Result<()> {
        if self.nonce == u64::MAX && self.weight != 0 {
            return Err(Error::InvalidPayload);
        }
        Ok(())
    }
}

/// `message.SubnetToL1Conversion` — the conversion-id hash payload
/// (`warp/message/subnet_to_l1_conversion.go`).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct SubnetToL1Conversion {
    /// `ID` — a hash of the conversion data.
    #[codec]
    pub id: Id,
}

/// `message.L1ValidatorRegistration` — a registration ack/nack
/// (`warp/message/l1_validator_registration.go`).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct L1ValidatorRegistration {
    /// `ValidationID` — the validator the registration refers to.
    #[codec]
    pub validation_id: Id,
    /// `Registered` — whether the validator is currently registered.
    #[codec]
    pub registered: bool,
}

/// `message` registry — the ACP-77 registry-payload interface (specs 20 §3.1).
#[derive(AvaCodec, Clone, Debug, PartialEq, Eq)]
#[codec(type_registry)]
pub enum RegistryPayload {
    /// `SubnetToL1Conversion` (type_id 0).
    #[codec(type_id = 0)]
    SubnetToL1Conversion(SubnetToL1Conversion),
    /// `RegisterL1Validator` (type_id 1).
    #[codec(type_id = 1)]
    RegisterL1Validator(RegisterL1Validator),
    /// `L1ValidatorRegistration` (type_id 2).
    #[codec(type_id = 2)]
    L1ValidatorRegistration(L1ValidatorRegistration),
    /// `L1ValidatorWeight` (type_id 3).
    #[codec(type_id = 3)]
    L1ValidatorWeight(L1ValidatorWeight),
}

impl Default for RegistryPayload {
    fn default() -> Self {
        RegistryPayload::RegisterL1Validator(RegisterL1Validator::default())
    }
}

impl RegistryPayload {
    /// Parses any registered ACP-77 registry payload from `bytes`.
    ///
    /// # Errors
    /// Returns a [`CodecError`](ava_codec::error::CodecError) on an unknown
    /// version/type, trailing bytes, or a short read.
    pub fn parse(bytes: &[u8]) -> CodecResult<Self> {
        let mut p = Self::default();
        registry_codec().unmarshal(bytes, &mut p)?;
        Ok(p)
    }

    /// `message.Initialize(p).Bytes()` — the marshaled registry-payload bytes.
    ///
    /// # Errors
    /// Returns a [`CodecError`](ava_codec::error::CodecError) on a codec write
    /// failure.
    pub fn marshal(&self) -> CodecResult<Vec<u8>> {
        registry_codec().marshal(CODEC_VERSION, self)
    }
}

/// The ACP-77 registry-payload codec manager (`warp/message/codec.go`).
fn registry_codec() -> &'static Manager {
    static M: OnceLock<Manager> = OnceLock::new();
    M.get_or_init(|| {
        let m = Manager::new(ava_codec::MAX_SLICE_LEN);
        let _ = m.register(CODEC_VERSION, Arc::new(LinearCodec::new()));
        m
    })
}
