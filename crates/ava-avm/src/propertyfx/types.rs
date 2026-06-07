// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `propertyfx` codec types (specs/09 §4.3).
//!
//! Registered type IDs (in the AVM codec):
//! - `MintOutput`    (15): `{ owners: OutputOwners }`
//! - `OwnedOutput`   (16): `{ owners: OutputOwners }` (structurally identical; distinct type)
//! - `MintOperation` (17): `{ mint_input: secp::Input, mint_output: MintOutput, owned_output: OwnedOutput }`
//! - `BurnOperation` (18): `{ input: secp::Input }` — burns, produces nothing
//! - `Credential`    (19): newtype around `secp::Credential`
//!
//! Wire encoding follows Go `codec.Manager.Marshal(0, …)` exactly:
//! 2-byte codec version `0x0000` + fields in declaration order.

use ava_codec::packer::Packer;
use ava_codec::{Deserializable, Serializable};
use ava_secp256k1fx::{Credential as SecpCredential, Input, OutputOwners};
use ava_vm::error::{Error, Result};

/// The codec version shared with `secp256k1fx` (`codecVersion == 0`).
pub const CODEC_VERSION: u16 = 0;

// ---------------------------------------------------------------------------
// MintOutput (typeID 15)
// ---------------------------------------------------------------------------

/// `propertyfx.MintOutput` — continuing mint authority.
///
/// Wire: `{ owners: OutputOwners }`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MintOutput {
    /// `Owners` — who may invoke the next mint.
    pub owners: OutputOwners,
}

impl MintOutput {
    /// Builds a [`MintOutput`].
    #[must_use]
    pub fn new(owners: OutputOwners) -> Self {
        Self { owners }
    }

    pub(crate) fn marshal_fields(&self, p: &mut Packer) {
        self.owners.marshal_into(p);
    }

    pub(crate) fn unmarshal_fields(p: &mut Packer) -> Result<Self> {
        let mut owners = OutputOwners::default();
        owners.unmarshal_from(p);
        if p.errored() {
            return Err(Error::InvalidComponent("malformed MintOutput owners"));
        }
        Ok(Self { owners })
    }
}

// ---------------------------------------------------------------------------
// OwnedOutput (typeID 16)
// ---------------------------------------------------------------------------

/// `propertyfx.OwnedOutput` — the output produced by a mint or held for burning.
///
/// Wire: `{ owners: OutputOwners }` — structurally identical to [`MintOutput`];
/// the distinction is the typeID registered in the VM codec.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OwnedOutput {
    /// `Owners` — who may spend or burn this output.
    pub owners: OutputOwners,
}

impl OwnedOutput {
    /// Builds an [`OwnedOutput`].
    #[must_use]
    pub fn new(owners: OutputOwners) -> Self {
        Self { owners }
    }

    pub(crate) fn marshal_fields(&self, p: &mut Packer) {
        self.owners.marshal_into(p);
    }

    pub(crate) fn unmarshal_fields(p: &mut Packer) -> Result<Self> {
        let mut owners = OutputOwners::default();
        owners.unmarshal_from(p);
        if p.errored() {
            return Err(Error::InvalidComponent("malformed OwnedOutput owners"));
        }
        Ok(Self { owners })
    }
}

// ---------------------------------------------------------------------------
// PropertyOutput — union of the two propertyfx output types
// ---------------------------------------------------------------------------

/// The outputs that a propertyfx operation can produce.
///
/// `MintOperation::outs()` returns `[PropertyOutput::Mint(..), PropertyOutput::Owned(..)]`;
/// `BurnOperation::outs()` returns `[]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropertyOutput {
    /// A `MintOutput` (continuing mint authority).
    Mint(MintOutput),
    /// An `OwnedOutput` (newly produced owned output).
    Owned(OwnedOutput),
}

// ---------------------------------------------------------------------------
// MintOperation (typeID 17)
// ---------------------------------------------------------------------------

/// `propertyfx.MintOperation` — mints a new `OwnedOutput` and continues mint
/// authority in a new `MintOutput`.
///
/// Wire: `{ mint_input: Input, mint_output: MintOutput, owned_output: OwnedOutput }`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MintOperation {
    /// `MintInput` — spend condition on the minting UTXO.
    pub mint_input: Input,
    /// `MintOutput` — new mint-authority output.
    pub mint_output: MintOutput,
    /// `OwnedOutput` — newly minted owned output.
    pub owned_output: OwnedOutput,
}

impl MintOperation {
    /// Builds a [`MintOperation`].
    #[must_use]
    pub fn new(mint_input: Input, mint_output: MintOutput, owned_output: OwnedOutput) -> Self {
        Self {
            mint_input,
            mint_output,
            owned_output,
        }
    }

    /// Returns the outputs produced by this operation: `[mint_output, owned_output]`.
    #[must_use]
    pub fn outs(&self) -> Vec<PropertyOutput> {
        vec![
            PropertyOutput::Mint(self.mint_output.clone()),
            PropertyOutput::Owned(self.owned_output.clone()),
        ]
    }

    pub(crate) fn marshal_fields(&self, p: &mut Packer) {
        self.mint_input.marshal_into(p);
        self.mint_output.marshal_fields(p);
        self.owned_output.marshal_fields(p);
    }

    pub(crate) fn unmarshal_fields(p: &mut Packer) -> Result<Self> {
        let mut mint_input = Input::default();
        mint_input.unmarshal_from(p);
        if p.errored() {
            return Err(Error::InvalidComponent(
                "malformed MintOperation mint_input",
            ));
        }
        let mint_output = MintOutput::unmarshal_fields(p)?;
        let owned_output = OwnedOutput::unmarshal_fields(p)?;
        Ok(Self {
            mint_input,
            mint_output,
            owned_output,
        })
    }
}

// ---------------------------------------------------------------------------
// BurnOperation (typeID 18)
// ---------------------------------------------------------------------------

/// `propertyfx.BurnOperation` — burns an `OwnedOutput`, producing nothing.
///
/// Wire: `{ input: Input }`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BurnOperation {
    /// `Input` — spend condition on the consumed `OwnedOutput`.
    pub input: Input,
}

impl BurnOperation {
    /// Builds a [`BurnOperation`].
    #[must_use]
    pub fn new(input: Input) -> Self {
        Self { input }
    }

    /// Returns the outputs produced by this operation: always empty (burns).
    #[must_use]
    pub fn outs(&self) -> Vec<PropertyOutput> {
        vec![]
    }

    pub(crate) fn marshal_fields(&self, p: &mut Packer) {
        self.input.marshal_into(p);
    }

    pub(crate) fn unmarshal_fields(p: &mut Packer) -> Result<Self> {
        let mut input = Input::default();
        input.unmarshal_from(p);
        if p.errored() {
            return Err(Error::InvalidComponent("malformed BurnOperation input"));
        }
        Ok(Self { input })
    }
}

// ---------------------------------------------------------------------------
// Credential (typeID 19)
// ---------------------------------------------------------------------------

/// `propertyfx.Credential` — newtype around `secp256k1fx.Credential`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Credential(pub SecpCredential);

impl Credential {
    /// Builds a [`Credential`].
    #[must_use]
    pub fn new(inner: SecpCredential) -> Self {
        Self(inner)
    }

    pub(crate) fn marshal_fields(&self, p: &mut Packer) {
        self.0.marshal_into(p);
    }

    pub(crate) fn unmarshal_fields(p: &mut Packer) -> Result<Self> {
        let mut inner = SecpCredential::default();
        inner.unmarshal_from(p);
        if p.errored() {
            return Err(Error::InvalidComponent("malformed propertyfx Credential"));
        }
        Ok(Self(inner))
    }
}

// ---------------------------------------------------------------------------
// Codec helpers — byte-exact with Go `codec.Manager.Marshal(0, …)`
// ---------------------------------------------------------------------------

/// A type that can be marshaled byte-exact under the propertyfx codec.
pub trait PropFxMarshal {
    /// Appends the `serialize:"true"` fields in declaration order.
    fn marshal_fields(&self, p: &mut Packer);
}

macro_rules! impl_propfx_marshal {
    ($($t:ty),+ $(,)?) => {$(
        impl PropFxMarshal for $t {
            fn marshal_fields(&self, p: &mut Packer) { <$t>::marshal_fields(self, p); }
        }
    )+};
}

impl_propfx_marshal!(
    MintOutput,
    OwnedOutput,
    MintOperation,
    BurnOperation,
    Credential
);

/// `codec.Manager.Marshal(0, v)` — 2-byte codec version prefix + the value's
/// `serialize:"true"` fields (no typeID).
#[must_use]
pub fn marshal<T: PropFxMarshal>(v: &T) -> Vec<u8> {
    let mut p = Packer::with_max_size(usize::MAX);
    p.pack_u16(CODEC_VERSION);
    v.marshal_fields(&mut p);
    p.into_bytes()
}

/// Reads the 2-byte codec version, asserts [`CODEC_VERSION`], then delegates.
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

/// Unmarshal a [`MintOutput`] from codec bytes.
///
/// # Errors
/// Returns [`Error::InvalidComponent`] on a malformed stream.
pub fn unmarshal_mint_output(bytes: &[u8]) -> Result<MintOutput> {
    unmarshal_with(bytes, MintOutput::unmarshal_fields)
}

/// Unmarshal an [`OwnedOutput`] from codec bytes.
///
/// # Errors
/// Returns [`Error::InvalidComponent`] on a malformed stream.
pub fn unmarshal_owned_output(bytes: &[u8]) -> Result<OwnedOutput> {
    unmarshal_with(bytes, OwnedOutput::unmarshal_fields)
}

/// Unmarshal a [`MintOperation`] from codec bytes.
///
/// # Errors
/// Returns [`Error::InvalidComponent`] on a malformed stream.
pub fn unmarshal_mint_operation(bytes: &[u8]) -> Result<MintOperation> {
    unmarshal_with(bytes, MintOperation::unmarshal_fields)
}

/// Unmarshal a [`BurnOperation`] from codec bytes.
///
/// # Errors
/// Returns [`Error::InvalidComponent`] on a malformed stream.
pub fn unmarshal_burn_operation(bytes: &[u8]) -> Result<BurnOperation> {
    unmarshal_with(bytes, BurnOperation::unmarshal_fields)
}

/// Unmarshal a [`Credential`] from codec bytes.
///
/// # Errors
/// Returns [`Error::InvalidComponent`] on a malformed stream.
pub fn unmarshal_credential(bytes: &[u8]) -> Result<Credential> {
    unmarshal_with(bytes, Credential::unmarshal_fields)
}

// ---------------------------------------------------------------------------
// `ava_codec` Serializable/Deserializable impls
// ---------------------------------------------------------------------------

macro_rules! impl_codec_traits {
    ($($t:ty),+ $(,)?) => {$(
        impl Serializable for $t {
            fn marshal_into(&self, p: &mut Packer) {
                <$t>::marshal_fields(self, p);
            }
            fn size(&self) -> usize {
                let mut sp = Packer::with_max_size(usize::MAX);
                <$t>::marshal_fields(self, &mut sp);
                sp.into_bytes().len()
            }
        }
        impl Deserializable for $t {
            fn unmarshal_from(&mut self, p: &mut Packer) {
                if p.errored() {
                    return;
                }
                match <$t>::unmarshal_fields(p) {
                    Ok(v) => *self = v,
                    Err(_) => p.add_external_error(ava_codec::error::PackerError::InvalidInput),
                }
            }
        }
    )+};
}

impl_codec_traits!(
    MintOutput,
    OwnedOutput,
    MintOperation,
    BurnOperation,
    Credential
);
