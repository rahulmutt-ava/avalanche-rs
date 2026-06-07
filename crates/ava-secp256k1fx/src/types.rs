// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `secp256k1fx` codec types (specs 07 §4.2).
//!
//! Registered into the VM codec in this exact order (typeID = index, per `03`):
//! `TransferInput`(0), `MintOutput`(1), `TransferOutput`(2), `MintOperation`(3),
//! `Credential`(4). Each type's `marshal_fields` emits the `serialize:"true"`
//! fields in declaration order; [`marshal`] prepends the 2-byte codec version
//! (`0x0000`) so the bytes are identical to Go's `codec.Manager.Marshal(0, …)`.

use ava_codec::packer::Packer;
use ava_crypto::secp256k1::SIGNATURE_LEN;
use ava_types::short_id::{SHORT_ID_LEN, ShortId};
use ava_utils::math as safemath;
use ava_vm::ChainContext;
use ava_vm::components::verify::{State, Verifiable};
use ava_vm::error::{Error, Result};

use crate::error::{ERR_INPUT_INDICES_NOT_SORTED_UNIQUE, ERR_NO_VALUE_INPUT, ERR_NO_VALUE_OUTPUT};

/// The codec version Avalanche marshals fx types under (`codecVersion == 0`).
pub const CODEC_VERSION: u16 = 0;

/// Returns `true` iff `s` is strictly increasing (sorted and unique).
fn is_sorted_and_unique<T: Ord>(s: &[T]) -> bool {
    s.is_sorted_by(|a, b| a < b)
}

// ---------------------------------------------------------------------------
// OutputOwners
// ---------------------------------------------------------------------------

/// `secp256k1fx.OutputOwners`. NOT a `verify::State` (Go `IsNotState`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutputOwners {
    /// `Locktime` (`serialize`) — unix seconds before which the output cannot be
    /// spent.
    pub locktime: u64,
    /// `Threshold` (`serialize`) — number of signatures required to spend.
    pub threshold: u32,
    /// `Addrs` (`serialize`) — the owning addresses; must be sorted and unique.
    pub addrs: Vec<ShortId>,
}

impl OutputOwners {
    /// Builds an [`OutputOwners`].
    #[must_use]
    pub fn new(locktime: u64, threshold: u32, addrs: Vec<ShortId>) -> Self {
        Self {
            locktime,
            threshold,
            addrs,
        }
    }

    /// `Equals` — true iff the two owners encode the same spend condition.
    #[must_use]
    pub fn equals(&self, other: &Self) -> bool {
        self == other
    }

    /// Appends the `serialize:"true"` fields (locktime, threshold, addrs) in
    /// declaration order.
    pub(crate) fn marshal_fields(&self, p: &mut Packer) {
        p.pack_u64(self.locktime);
        p.pack_u32(self.threshold);
        // `[]ids.ShortID` is a non-`[]byte` slice: u32 count + each 20-byte addr.
        p.pack_u32(self.addrs.len() as u32);
        for addr in &self.addrs {
            p.pack_fixed_bytes(addr.as_bytes());
        }
    }

    /// Reads the `serialize:"true"` fields from `p`.
    pub(crate) fn unmarshal_fields(p: &mut Packer) -> Result<Self> {
        let locktime = p.unpack_u64();
        let threshold = p.unpack_u32();
        let n = p.unpack_u32() as usize;
        // Guard against a truncated/oversized count: bail before allocating per
        // element so an attacker-controlled `n` cannot drive unbounded `Vec`
        // growth (decode-never-OOMs, specs 02 §8; mirrors the P-Chain decoders).
        if p.errored() {
            return Err(Error::InvalidComponent("truncated output owners"));
        }
        let mut addrs = Vec::with_capacity(n.min(ava_codec::INITIAL_SLICE_CAP));
        for _ in 0..n {
            let b = p.unpack_fixed_bytes(SHORT_ID_LEN);
            if p.errored() {
                return Err(Error::InvalidComponent("truncated output owners"));
            }
            let addr = ShortId::from_slice(&b)
                .map_err(|_| Error::InvalidComponent("invalid short id length"))?;
            addrs.push(addr);
        }
        Ok(Self {
            locktime,
            threshold,
            addrs,
        })
    }
}

impl Verifiable for OutputOwners {
    /// `OutputOwners.Verify` — reproduced exactly (07 §4.2).
    fn verify(&self) -> Result<()> {
        if self.threshold > self.addrs.len() as u32 {
            return Err(Error::OutputUnspendable);
        }
        if self.threshold == 0 && !self.addrs.is_empty() {
            return Err(Error::OutputUnoptimized);
        }
        if !is_sorted_and_unique(&self.addrs) {
            return Err(Error::AddrsNotSortedUnique);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

/// `secp256k1fx.Input` — the `SigIndices` common to every spend.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Input {
    /// `SigIndices` (`serialize`) — owner-address indices; sorted and unique.
    pub sig_indices: Vec<u32>,
}

impl Input {
    /// `CostPerSignature` — gas cost charged per signature.
    pub const COST_PER_SIGNATURE: u64 = 1000;

    /// Builds an [`Input`].
    #[must_use]
    pub fn new(sig_indices: Vec<u32>) -> Self {
        Self { sig_indices }
    }

    /// `Cost` — `len(SigIndices) * CostPerSignature`, checked.
    ///
    /// # Errors
    /// Returns [`Error::Overflow`] if the multiplication overflows `u64`.
    pub fn cost(&self) -> Result<u64> {
        safemath::mul(self.sig_indices.len() as u64, Self::COST_PER_SIGNATURE)
            .map_err(|_| Error::Overflow)
    }

    pub(crate) fn marshal_fields(&self, p: &mut Packer) {
        // `[]uint32`: u32 count + each big-endian u32.
        p.pack_u32(self.sig_indices.len() as u32);
        for idx in &self.sig_indices {
            p.pack_u32(*idx);
        }
    }

    pub(crate) fn unmarshal_fields(p: &mut Packer) -> Result<Self> {
        let n = p.unpack_u32() as usize;
        // Bail on a truncated count before allocating per element (decode-never-
        // OOMs, specs 02 §8; mirrors the P-Chain decoders).
        if p.errored() {
            return Err(Error::InvalidComponent("truncated input"));
        }
        let mut sig_indices = Vec::with_capacity(n.min(ava_codec::INITIAL_SLICE_CAP));
        for _ in 0..n {
            let idx = p.unpack_u32();
            if p.errored() {
                return Err(Error::InvalidComponent("truncated input"));
            }
            sig_indices.push(idx);
        }
        Ok(Self { sig_indices })
    }
}

impl Verifiable for Input {
    /// `Input.Verify` — sig indices must be sorted and unique.
    fn verify(&self) -> Result<()> {
        if !is_sorted_and_unique(&self.sig_indices) {
            return Err(Error::InvalidComponent(ERR_INPUT_INDICES_NOT_SORTED_UNIQUE));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// TransferInput (typeID 0)
// ---------------------------------------------------------------------------

/// `secp256k1fx.TransferInput`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransferInput {
    /// `Amt` (`serialize`) — the quantity of the asset this input produces.
    pub amt: u64,
    /// Embedded `Input` (`serialize`).
    pub input: Input,
}

impl TransferInput {
    /// Builds a [`TransferInput`].
    #[must_use]
    pub fn new(amt: u64, sig_indices: Vec<u32>) -> Self {
        Self {
            amt,
            input: Input::new(sig_indices),
        }
    }

    pub(crate) fn marshal_fields(&self, p: &mut Packer) {
        p.pack_u64(self.amt);
        self.input.marshal_fields(p);
    }

    pub(crate) fn unmarshal_fields(p: &mut Packer) -> Result<Self> {
        let amt = p.unpack_u64();
        let input = Input::unmarshal_fields(p)?;
        Ok(Self { amt, input })
    }
}

impl Verifiable for TransferInput {
    /// `TransferInput.Verify` — non-zero amount, then `Input.Verify`.
    fn verify(&self) -> Result<()> {
        if self.amt == 0 {
            return Err(Error::InvalidComponent(ERR_NO_VALUE_INPUT));
        }
        self.input.verify()
    }
}

// ---------------------------------------------------------------------------
// MintOutput (typeID 1)
// ---------------------------------------------------------------------------

/// `secp256k1fx.MintOutput` (a `verify::State`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MintOutput {
    /// Embedded `OutputOwners` (`serialize`).
    pub owners: OutputOwners,
}

impl MintOutput {
    /// Builds a [`MintOutput`].
    #[must_use]
    pub fn new(owners: OutputOwners) -> Self {
        Self { owners }
    }

    pub(crate) fn marshal_fields(&self, p: &mut Packer) {
        self.owners.marshal_fields(p);
    }

    pub(crate) fn unmarshal_fields(p: &mut Packer) -> Result<Self> {
        Ok(Self {
            owners: OutputOwners::unmarshal_fields(p)?,
        })
    }
}

impl Verifiable for MintOutput {
    fn verify(&self) -> Result<()> {
        self.owners.verify()
    }
}

impl State for MintOutput {
    fn init_ctx(&self, _ctx: &ChainContext) {}
}

// ---------------------------------------------------------------------------
// TransferOutput (typeID 2)
// ---------------------------------------------------------------------------

/// `secp256k1fx.TransferOutput` (a `verify::State`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransferOutput {
    /// `Amt` (`serialize`) — the quantity of the asset this output holds.
    pub amt: u64,
    /// Embedded `OutputOwners` (`serialize`).
    pub owners: OutputOwners,
}

impl TransferOutput {
    /// Builds a [`TransferOutput`].
    #[must_use]
    pub fn new(amt: u64, owners: OutputOwners) -> Self {
        Self { amt, owners }
    }

    pub(crate) fn marshal_fields(&self, p: &mut Packer) {
        p.pack_u64(self.amt);
        self.owners.marshal_fields(p);
    }

    pub(crate) fn unmarshal_fields(p: &mut Packer) -> Result<Self> {
        let amt = p.unpack_u64();
        let owners = OutputOwners::unmarshal_fields(p)?;
        Ok(Self { amt, owners })
    }
}

impl Verifiable for TransferOutput {
    /// `TransferOutput.Verify` — non-zero amount, then `OutputOwners.Verify`.
    fn verify(&self) -> Result<()> {
        if self.amt == 0 {
            return Err(Error::InvalidComponent(ERR_NO_VALUE_OUTPUT));
        }
        self.owners.verify()
    }
}

impl State for TransferOutput {
    fn init_ctx(&self, _ctx: &ChainContext) {}
}

// ---------------------------------------------------------------------------
// Credential (typeID 4)
// ---------------------------------------------------------------------------

/// `secp256k1fx.Credential` — fixed-size 65-byte recoverable sigs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Credential {
    /// `Sigs` (`serialize`) — one 65-byte `[r||s||v]` signature per spend.
    pub sigs: Vec<[u8; SIGNATURE_LEN]>,
}

impl Credential {
    /// Builds a [`Credential`].
    #[must_use]
    pub fn new(sigs: Vec<[u8; SIGNATURE_LEN]>) -> Self {
        Self { sigs }
    }

    pub(crate) fn marshal_fields(&self, p: &mut Packer) {
        // `[][65]byte`: u32 count + each fixed 65-byte array (no inner prefix).
        p.pack_u32(self.sigs.len() as u32);
        for sig in &self.sigs {
            p.pack_fixed_bytes(sig);
        }
    }

    pub(crate) fn unmarshal_fields(p: &mut Packer) -> Result<Self> {
        let n = p.unpack_u32() as usize;
        // Bail on a truncated count before allocating per element (decode-never-
        // OOMs, specs 02 §8; mirrors the P-Chain decoders).
        if p.errored() {
            return Err(Error::InvalidComponent("truncated credential"));
        }
        let mut sigs = Vec::with_capacity(n.min(ava_codec::INITIAL_SLICE_CAP));
        for _ in 0..n {
            let b = p.unpack_fixed_bytes(SIGNATURE_LEN);
            if p.errored() {
                return Err(Error::InvalidComponent("truncated credential"));
            }
            let mut sig = [0u8; SIGNATURE_LEN];
            sig.copy_from_slice(&b);
            sigs.push(sig);
        }
        Ok(Self { sigs })
    }
}

impl Verifiable for Credential {
    fn verify(&self) -> Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Codec marshalling — byte-exact with Go `codec.Manager.Marshal(0, …)`
// ---------------------------------------------------------------------------

/// The fx codec typeIDs in registration order (07 §4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeId {
    /// `TransferInput`.
    TransferInput = 0,
    /// `MintOutput`.
    MintOutput = 1,
    /// `TransferOutput`.
    TransferOutput = 2,
    /// `MintOperation` (not yet a Rust type; reserved typeID).
    MintOperation = 3,
    /// `Credential`.
    Credential = 4,
}

/// A value that can be marshaled byte-exact under the fx codec.
pub trait FxMarshal {
    /// Appends the `serialize:"true"` fields in declaration order.
    fn marshal_fields(&self, p: &mut Packer);
}

macro_rules! impl_fx_marshal {
    ($($t:ty),+ $(,)?) => {$(
        impl FxMarshal for $t {
            fn marshal_fields(&self, p: &mut Packer) { <$t>::marshal_fields(self, p); }
        }
    )+};
}
impl_fx_marshal!(
    OutputOwners,
    Input,
    TransferInput,
    MintOutput,
    TransferOutput,
    Credential
);

/// `codec.Manager.Marshal(0, v)` — 2-byte codec version prefix + the value's
/// `serialize:"true"` fields (no typeID — matches the Go unit-test vectors which
/// marshal the concrete type directly).
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
/// Returns [`Error::InvalidComponent`] if the version is unexpected.
fn unmarshal_with<F, T>(bytes: &[u8], read: F) -> Result<T>
where
    F: FnOnce(&mut Packer<'_>) -> Result<T>,
{
    let mut p = Packer::new_read(bytes);
    let version = p.unpack_u16();
    if version != CODEC_VERSION {
        return Err(Error::InvalidComponent("unknown codec version"));
    }
    read(&mut p)
}

/// `codec.Manager.Unmarshal` for [`TransferInput`].
///
/// # Errors
/// Returns [`Error::InvalidComponent`] on a malformed stream.
pub fn unmarshal_transfer_input(bytes: &[u8]) -> Result<TransferInput> {
    unmarshal_with(bytes, TransferInput::unmarshal_fields)
}

/// `codec.Manager.Unmarshal` for [`MintOutput`].
///
/// # Errors
/// Returns [`Error::InvalidComponent`] on a malformed stream.
pub fn unmarshal_mint_output(bytes: &[u8]) -> Result<MintOutput> {
    unmarshal_with(bytes, MintOutput::unmarshal_fields)
}

/// `codec.Manager.Unmarshal` for [`TransferOutput`].
///
/// # Errors
/// Returns [`Error::InvalidComponent`] on a malformed stream.
pub fn unmarshal_transfer_output(bytes: &[u8]) -> Result<TransferOutput> {
    unmarshal_with(bytes, TransferOutput::unmarshal_fields)
}

/// `codec.Manager.Unmarshal` for [`OutputOwners`].
///
/// # Errors
/// Returns [`Error::InvalidComponent`] on a malformed stream.
pub fn unmarshal_output_owners(bytes: &[u8]) -> Result<OutputOwners> {
    unmarshal_with(bytes, OutputOwners::unmarshal_fields)
}

/// `codec.Manager.Unmarshal` for [`Input`].
///
/// # Errors
/// Returns [`Error::InvalidComponent`] on a malformed stream.
pub fn unmarshal_input(bytes: &[u8]) -> Result<Input> {
    unmarshal_with(bytes, Input::unmarshal_fields)
}

/// `codec.Manager.Unmarshal` for [`Credential`].
///
/// # Errors
/// Returns [`Error::InvalidComponent`] on a malformed stream.
pub fn unmarshal_credential(bytes: &[u8]) -> Result<Credential> {
    unmarshal_with(bytes, Credential::unmarshal_fields)
}

// ---------------------------------------------------------------------------
// `ava_codec` trait impls — embed fx types directly as codec fields
// ---------------------------------------------------------------------------
//
// The fx types are interface payloads in Go (`verify.State`, `fx.Owner`,
// `verify.Verifiable`). When a P-Chain / X-Chain tx embeds one through such an
// interface, the codec prepends the registered typeID; that typeID prefix is
// emitted by the **embedding** interface enum (e.g. `ava_platformvm::txs::Output`),
// not here. These `Serializable`/`Deserializable` impls therefore write/read
// only the `serialize:"true"` fields (byte-identical to `marshal_fields`), with
// no version prefix and no typeID — the building block the interface enums and
// the `stakeable` lock wrappers compose. (Promoting these to the public
// `ava_codec` traits is the M4.3 resolution recorded in specs/08 §2.3, replacing
// the per-crate byte-exact mirrors.)

/// Bridges a `fn(&mut Packer) -> Result<Self>` field reader into the codec's
/// in-place [`ava_codec::Deserializable`] contract, surfacing the fx error as a
/// sticky packer error.
fn read_fields<F, T>(p: &mut Packer, read: F, dst: &mut T)
where
    F: FnOnce(&mut Packer) -> Result<T>,
{
    if p.errored() {
        return;
    }
    match read(p) {
        Ok(v) => *dst = v,
        Err(_) => p.add_external_error(ava_codec::error::PackerError::InvalidInput),
    }
}

macro_rules! impl_codec_traits {
    ($($t:ty),+ $(,)?) => {$(
        impl ava_codec::Serializable for $t {
            fn marshal_into(&self, p: &mut Packer) {
                <$t>::marshal_fields(self, p);
            }

            fn size(&self) -> usize {
                // The exact field byte length: marshal into a sizing packer.
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
    OutputOwners,
    Input,
    TransferInput,
    MintOutput,
    TransferOutput,
    Credential,
);
