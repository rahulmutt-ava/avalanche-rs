// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `nftfx` codec types (specs/09 §4.2).
//!
//! Registered into the VM codec in typeID order (10–14):
//! `MintOutput`(10), `TransferOutput`(11), `MintOperation`(12),
//! `TransferOperation`(13), `Credential`(14).
//!
//! Field order = serialization order per spec §4.2. Each type's
//! `marshal_fields` emits the `serialize:"true"` fields in declaration order;
//! [`marshal`] prepends the 2-byte codec version (`0x0000`) so the bytes are
//! identical to Go's `codec.Manager.Marshal(0, …)`.

use ava_codec::packer::Packer;
use ava_codec::{Deserializable, Serializable};
use ava_vm::ChainContext;
use ava_vm::components::verify::{State, Verifiable};
use ava_vm::error::{Error as VmError, Result as VmResult};

use ava_secp256k1fx::types::{Credential as SecpCredential, Input, OutputOwners};

use crate::Error;

/// The codec version Avalanche marshals fx types under (`codecVersion == 0`).
pub const CODEC_VERSION: u16 = 0;

/// Maximum payload size for `nftfx::TransferOutput` (1 KiB, per specs/09 §4.2).
pub const MAX_PAYLOAD_SIZE: usize = 1024;

// ---------------------------------------------------------------------------
// Helper — bridge `fn(&mut Packer) -> VmResult<Self>` → `Deserializable`.
// ---------------------------------------------------------------------------

fn read_fields<F, T>(p: &mut Packer, read: F, dst: &mut T)
where
    F: FnOnce(&mut Packer) -> VmResult<T>,
{
    if p.errored() {
        return;
    }
    match read(p) {
        Ok(v) => *dst = v,
        Err(_) => p.add_external_error(ava_codec::error::PackerError::InvalidInput),
    }
}

// ---------------------------------------------------------------------------
// MintOutput (typeID 10)
// ---------------------------------------------------------------------------

/// `nftfx.MintOutput` — minting authority for a group.
///
/// Wire fields (field order = serialization order):
/// - `group_id: u32`
/// - `owners: OutputOwners`
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MintOutput {
    /// `GroupID` (`serialize`) — the NFT group this output can mint into.
    pub group_id: u32,
    /// Embedded `OutputOwners` (`serialize`).
    pub owners: OutputOwners,
}

impl MintOutput {
    /// Builds a [`MintOutput`].
    #[must_use]
    pub fn new(group_id: u32, owners: OutputOwners) -> Self {
        Self { group_id, owners }
    }

    pub(crate) fn marshal_fields(&self, p: &mut Packer) {
        p.pack_u32(self.group_id);
        self.owners.marshal_into(p);
    }

    pub(crate) fn unmarshal_fields(p: &mut Packer) -> VmResult<Self> {
        let group_id = p.unpack_u32();
        let mut owners = OutputOwners::default();
        owners.unmarshal_from(p);
        if p.errored() {
            return Err(VmError::InvalidComponent(
                "malformed nftfx MintOutput owners",
            ));
        }
        Ok(Self { group_id, owners })
    }
}

impl Verifiable for MintOutput {
    fn verify(&self) -> VmResult<()> {
        self.owners.verify()
    }
}

impl State for MintOutput {
    fn init_ctx(&self, _ctx: &ChainContext) {}
}

// ---------------------------------------------------------------------------
// TransferOutput (typeID 11)
// ---------------------------------------------------------------------------

/// `nftfx.TransferOutput` — an NFT with a payload, owned by `owners`.
///
/// Wire fields:
/// - `group_id: u32`
/// - `payload: Vec<u8>` (`<= 1 KiB`)
/// - `owners: OutputOwners`
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransferOutput {
    /// `GroupID` (`serialize`) — the NFT group this output belongs to.
    pub group_id: u32,
    /// `Payload` (`serialize`) — opaque bytes, at most 1 KiB.
    pub payload: Vec<u8>,
    /// Embedded `OutputOwners` (`serialize`).
    pub owners: OutputOwners,
}

impl TransferOutput {
    /// Builds a [`TransferOutput`].
    #[must_use]
    pub fn new(group_id: u32, payload: Vec<u8>, owners: OutputOwners) -> Self {
        Self {
            group_id,
            payload,
            owners,
        }
    }

    pub(crate) fn marshal_fields(&self, p: &mut Packer) {
        p.pack_u32(self.group_id);
        // `[]byte` → u32 length prefix + raw bytes.
        ava_codec::pack_count(p, self.payload.len());
        p.pack_fixed_bytes(&self.payload);
        self.owners.marshal_into(p);
    }

    pub(crate) fn unmarshal_fields(p: &mut Packer) -> VmResult<Self> {
        let group_id = p.unpack_u32();
        let len = p.unpack_u32() as usize;
        let raw = p.unpack_fixed_bytes(len);
        let payload = raw.to_vec();
        let mut owners = OutputOwners::default();
        owners.unmarshal_from(p);
        if p.errored() {
            return Err(VmError::InvalidComponent(
                "malformed nftfx TransferOutput owners",
            ));
        }
        Ok(Self {
            group_id,
            payload,
            owners,
        })
    }
}

impl Verifiable for TransferOutput {
    /// `TransferOutput.Verify` — owners verify + payload ≤ 1 KiB.
    fn verify(&self) -> VmResult<()> {
        if self.payload.len() > MAX_PAYLOAD_SIZE {
            return Err(VmError::InvalidComponent(
                "nftfx payload exceeds maximum size",
            ));
        }
        self.owners.verify()
    }
}

// Map the ava_avm::Error::PayloadTooLarge onto VmError; also expose a
// convenience helper that returns our crate's Result.
impl TransferOutput {
    /// Validates this output, returning [`crate::Error::PayloadTooLarge`] on
    /// an oversized payload rather than the vm-level sentinel.
    ///
    /// # Errors
    /// Returns [`crate::Error::PayloadTooLarge`] when `payload.len() > 1024`,
    /// or the [`ava_vm::error::Error`] from `owners.verify()`.
    pub fn avm_verify(&self) -> crate::Result<()> {
        if self.payload.len() > MAX_PAYLOAD_SIZE {
            return Err(Error::PayloadTooLarge);
        }
        self.owners.verify().map_err(Error::Fx)
    }
}

impl State for TransferOutput {
    fn init_ctx(&self, _ctx: &ChainContext) {}
}

// ---------------------------------------------------------------------------
// MintOperation (typeID 12)
// ---------------------------------------------------------------------------

/// `nftfx.MintOperation` — mints new NFTs into `outputs`.
///
/// Wire fields:
/// - `mint_input: secp::Input`
/// - `group_id: u32`
/// - `payload: Vec<u8>`
/// - `outputs: Vec<OutputOwners>`
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MintOperation {
    /// `MintInput` (`serialize`) — the spend credential for the minting UTXO.
    pub mint_input: Input,
    /// `GroupID` (`serialize`) — the NFT group being minted into.
    pub group_id: u32,
    /// `Payload` (`serialize`) — opaque payload for every minted NFT.
    pub payload: Vec<u8>,
    /// `Outputs` (`serialize`) — one `OutputOwners` per new NFT being minted.
    pub outputs: Vec<OutputOwners>,
}

impl MintOperation {
    /// Synthesizes one [`TransferOutput`] per entry in `outputs`, all sharing
    /// the operation's `group_id` and `payload` (specs/09 §4.2).
    #[must_use]
    pub fn outs(&self) -> Vec<TransferOutput> {
        self.outputs
            .iter()
            .map(|owners| TransferOutput {
                group_id: self.group_id,
                payload: self.payload.clone(),
                owners: owners.clone(),
            })
            .collect()
    }

    pub(crate) fn marshal_fields(&self, p: &mut Packer) {
        self.mint_input.marshal_into(p);
        p.pack_u32(self.group_id);
        ava_codec::pack_count(p, self.payload.len());
        p.pack_fixed_bytes(&self.payload);
        // `outputs: Vec<OutputOwners>` — u32 count + each OutputOwners.
        ava_codec::pack_count(p, self.outputs.len());
        for owners in &self.outputs {
            owners.marshal_into(p);
        }
    }

    pub(crate) fn unmarshal_fields(p: &mut Packer) -> VmResult<Self> {
        let mut mint_input = Input::default();
        mint_input.unmarshal_from(p);
        if p.errored() {
            return Err(VmError::InvalidComponent(
                "malformed nftfx MintOperation mint_input",
            ));
        }
        let group_id = p.unpack_u32();
        let plen = p.unpack_u32() as usize;
        let raw = p.unpack_fixed_bytes(plen);
        let payload = raw.to_vec();
        let n = p.unpack_u32() as usize;
        let mut outputs = Vec::with_capacity(n.min(ava_codec::INITIAL_SLICE_CAP));
        for _ in 0..n {
            let mut owners = OutputOwners::default();
            owners.unmarshal_from(p);
            if p.errored() {
                return Err(VmError::InvalidComponent(
                    "malformed nftfx MintOperation outputs",
                ));
            }
            outputs.push(owners);
        }
        Ok(Self {
            mint_input,
            group_id,
            payload,
            outputs,
        })
    }
}

impl Verifiable for MintOperation {
    fn verify(&self) -> VmResult<()> {
        self.mint_input.verify()?;
        for owners in &self.outputs {
            owners.verify()?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// TransferOperation (typeID 13)
// ---------------------------------------------------------------------------

/// `nftfx.TransferOperation` — transfers an existing NFT.
///
/// Wire fields:
/// - `input: secp::Input`
/// - `output: TransferOutput`
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransferOperation {
    /// `Input` (`serialize`) — the spend credential for the NFT UTXO.
    pub input: Input,
    /// `Output` (`serialize`) — the new NFT output.
    pub output: TransferOutput,
}

impl TransferOperation {
    /// Returns a single-element vec wrapping the operation's output.
    #[must_use]
    pub fn outs(&self) -> Vec<TransferOutput> {
        vec![self.output.clone()]
    }

    pub(crate) fn marshal_fields(&self, p: &mut Packer) {
        self.input.marshal_into(p);
        self.output.marshal_fields(p);
    }

    pub(crate) fn unmarshal_fields(p: &mut Packer) -> VmResult<Self> {
        let mut input = Input::default();
        input.unmarshal_from(p);
        if p.errored() {
            return Err(VmError::InvalidComponent(
                "malformed nftfx TransferOperation input",
            ));
        }
        let output = TransferOutput::unmarshal_fields(p)?;
        Ok(Self { input, output })
    }
}

impl Verifiable for TransferOperation {
    fn verify(&self) -> VmResult<()> {
        self.input.verify()?;
        self.output.verify()
    }
}

// ---------------------------------------------------------------------------
// Credential (typeID 14)
// ---------------------------------------------------------------------------

/// `nftfx.Credential` — a newtype around `secp256k1fx.Credential`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Credential(pub SecpCredential);

impl Credential {
    pub(crate) fn marshal_fields(&self, p: &mut Packer) {
        self.0.marshal_into(p);
    }

    pub(crate) fn unmarshal_fields(p: &mut Packer) -> VmResult<Self> {
        let mut inner = SecpCredential::default();
        inner.unmarshal_from(p);
        if p.errored() {
            return Err(VmError::InvalidComponent("malformed nftfx Credential"));
        }
        Ok(Self(inner))
    }
}

impl Verifiable for Credential {
    fn verify(&self) -> VmResult<()> {
        self.0.verify()
    }
}

// ---------------------------------------------------------------------------
// FxMarshal trait + top-level marshal/unmarshal helpers
// ---------------------------------------------------------------------------

/// A value that can be marshaled byte-exact under the nftfx codec.
pub trait FxMarshal {
    /// Appends the `serialize:"true"` fields in declaration order.
    fn marshal_fields(&self, p: &mut Packer);
}

macro_rules! impl_fx_marshal {
    ($($t:ty),+ $(,)?) => {$(
        impl FxMarshal for $t {
            fn marshal_fields(&self, p: &mut Packer) {
                <$t>::marshal_fields(self, p);
            }
        }
    )+};
}

impl_fx_marshal!(
    MintOutput,
    TransferOutput,
    MintOperation,
    TransferOperation,
    Credential
);

/// `codec.Manager.Marshal(0, v)` — 2-byte codec version prefix + the value's
/// `serialize:"true"` fields (no typeID).
#[must_use]
pub fn marshal<T: FxMarshal>(v: &T) -> Vec<u8> {
    let mut p = Packer::with_max_size(usize::MAX);
    p.pack_u16(CODEC_VERSION);
    v.marshal_fields(&mut p);
    p.into_bytes()
}

/// Reads the 2-byte codec version, asserts it is [`CODEC_VERSION`], then runs
/// `read` over the remaining bytes.
///
/// # Errors
/// Returns [`VmError::InvalidComponent`] if the version is unexpected.
fn unmarshal_with<F, T>(bytes: &[u8], read: F) -> VmResult<T>
where
    F: FnOnce(&mut Packer<'_>) -> VmResult<T>,
{
    let mut p = Packer::new_read(bytes);
    let version = p.unpack_u16();
    if version != CODEC_VERSION {
        return Err(VmError::InvalidComponent("unknown codec version"));
    }
    read(&mut p)
}

/// `codec.Manager.Unmarshal` for [`MintOutput`].
///
/// # Errors
/// Returns [`VmError::InvalidComponent`] on a malformed stream.
pub fn unmarshal_mint_output(bytes: &[u8]) -> VmResult<MintOutput> {
    unmarshal_with(bytes, MintOutput::unmarshal_fields)
}

/// `codec.Manager.Unmarshal` for [`TransferOutput`].
///
/// # Errors
/// Returns [`VmError::InvalidComponent`] on a malformed stream.
pub fn unmarshal_transfer_output(bytes: &[u8]) -> VmResult<TransferOutput> {
    unmarshal_with(bytes, TransferOutput::unmarshal_fields)
}

/// `codec.Manager.Unmarshal` for [`MintOperation`].
///
/// # Errors
/// Returns [`VmError::InvalidComponent`] on a malformed stream.
pub fn unmarshal_mint_operation(bytes: &[u8]) -> VmResult<MintOperation> {
    unmarshal_with(bytes, MintOperation::unmarshal_fields)
}

/// `codec.Manager.Unmarshal` for [`TransferOperation`].
///
/// # Errors
/// Returns [`VmError::InvalidComponent`] on a malformed stream.
pub fn unmarshal_transfer_operation(bytes: &[u8]) -> VmResult<TransferOperation> {
    unmarshal_with(bytes, TransferOperation::unmarshal_fields)
}

/// `codec.Manager.Unmarshal` for [`Credential`].
///
/// # Errors
/// Returns [`VmError::InvalidComponent`] on a malformed stream.
pub fn unmarshal_credential(bytes: &[u8]) -> VmResult<Credential> {
    unmarshal_with(bytes, Credential::unmarshal_fields)
}

// ---------------------------------------------------------------------------
// `ava_codec` trait impls — embed nftfx types directly as codec fields
// ---------------------------------------------------------------------------

macro_rules! impl_codec_traits {
    ($($t:ty),+ $(,)?) => {$(
        impl ava_codec::Serializable for $t {
            fn marshal_into(&self, p: &mut Packer) {
                <$t>::marshal_fields(self, p);
            }

            fn size(&self) -> usize {
                let mut sp = Packer::with_max_size(usize::MAX);
                <$t>::marshal_fields(self, &mut sp);
                sp.into_bytes().len()
            }
        }

        impl ava_codec::Deserializable for $t {
            fn unmarshal_from(&mut self, p: &mut Packer) {
                read_fields(p, <$t>::unmarshal_fields, self);
            }
        }
    )+};
}

impl_codec_traits!(
    MintOutput,
    TransferOutput,
    MintOperation,
    TransferOperation,
    Credential,
);
