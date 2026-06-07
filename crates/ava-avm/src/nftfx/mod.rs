// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `nftfx` — the Non-Fungible Token feature extension (specs/09 §4.2).
//!
//! Provides codec types for NFT minting and transfer:
//! `MintOutput`(10), `TransferOutput`(11), `MintOperation`(12),
//! `TransferOperation`(13), `Credential`(14).
//!
//! `secp256k1fx`'s [`Input`] and [`OutputOwners`] are embedded here and
//! re-exported for convenience.

pub mod types;

pub use types::{
    CODEC_VERSION, Credential, FxMarshal, MAX_PAYLOAD_SIZE, MintOperation, MintOutput,
    TransferOperation, TransferOutput, marshal, unmarshal_credential, unmarshal_mint_operation,
    unmarshal_mint_output, unmarshal_transfer_operation, unmarshal_transfer_output,
};
