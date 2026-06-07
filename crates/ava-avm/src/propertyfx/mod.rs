// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `propertyfx` — the property feature extension for the AVM (X-Chain).
//!
//! Implements the five wire types registered in the AVM codec at type IDs 15–19
//! (specs/09 §4.3): [`MintOutput`], [`OwnedOutput`], [`MintOperation`],
//! [`BurnOperation`], [`Credential`].
//!
//! All types implement [`ava_codec::Serializable`] / [`ava_codec::Deserializable`]
//! so they can be embedded as fields in AVM tx codec enums without a version
//! prefix.  The free [`marshal`] / `unmarshal_*` helpers wrap a type with the
//! standard `0x0000` codec-version prefix for standalone round-trip testing.

pub mod fx;
mod types;

pub use fx::{Fx, PropertyOperation, PropertyUtxo};
pub use types::{
    BurnOperation, Credential, MintOperation, MintOutput, OwnedOutput, PropFxMarshal,
    PropertyOutput, marshal, unmarshal_burn_operation, unmarshal_credential,
    unmarshal_mint_operation, unmarshal_mint_output, unmarshal_owned_output,
};
