// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-secp256k1fx` — the secp256k1 feature extension (`vms/secp256k1fx`).
//!
//! Tier T3 (VM framework). Owning spec: `specs/07-vm-framework.md` §4. Provides
//! the byte-exact codec types ([`OutputOwners`]/[`Input`]/[`TransferInput`]/
//! [`TransferOutput`]/[`MintOutput`]/[`Credential`]) registered into the VM codec
//! in typeID order `TransferInput`(0)/`MintOutput`(1)/`TransferOutput`(2)/
//! `MintOperation`(3)/`Credential`(4), and the multisig spend gate
//! [`Fx::verify_credentials`] (specs 07 §4.3) reproduced bit-for-bit with Go.
//!
//! The fx wrong-type / verification sentinels live on the shared
//! [`ava_vm::error::Error`] enum and are re-exported via [`error`].

#![forbid(unsafe_code)]

// Dev-dependencies are exercised only by the integration tests
// (`tests/golden_codec.rs`, `tests/prop_multisig.rs`); silence the
// `unused_crate_dependencies` lint for the lib-test compilation unit.
#[cfg(test)]
use assert_matches as _;
#[cfg(test)]
use hex as _;
#[cfg(test)]
use proptest as _;
#[cfg(test)]
use serde as _;
#[cfg(test)]
use serde_json as _;

pub mod error;
pub mod fx;
pub mod instance;
pub mod types;

pub use error::{Error, Result};
pub use fx::{Fx, UnsignedTx};
pub use instance::Secp256k1Fx;
pub use types::{
    CODEC_VERSION, Credential, FxMarshal, Input, MintOutput, OutputOwners, TransferInput,
    TransferOutput, TypeId, marshal, unmarshal_credential, unmarshal_input, unmarshal_mint_output,
    unmarshal_output_owners, unmarshal_transfer_input, unmarshal_transfer_output,
};
