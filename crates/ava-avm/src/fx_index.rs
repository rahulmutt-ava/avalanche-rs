// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `FxIndex` — the feature-extension index, assigned in VM-registration order
//! (specs/09 §2.2).
//!
//! Go's AVM passes its fxs to the VM in the order `secp256k1fx`, `nftfx`,
//! `propertyfx`; the index is **stable and on-disk-meaningful** — it appears in
//! `CreateAssetTx::InitialState.fx_index` — so the discriminants are protocol
//! constants, not an implementation detail.

/// Feature-extension index in VM-registration order (specs/09 §2.2).
///
/// The `#[repr(u32)]` discriminants are load-bearing: `InitialState` serializes
/// `fx_index` as a `u32`, and the verifier routes an output/input/operation/
/// credential to the matching fx by looking up its registered `TypeId` in the
/// `TypeToFxIndex` table.
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FxIndex {
    /// The base secp256k1 fx (transfer / mint of fungible assets).
    Secp256k1 = 0,
    /// Non-fungible-token fx.
    Nft = 1,
    /// Property fx (mint/own/burn opaque ownership claims).
    Property = 2,
}
