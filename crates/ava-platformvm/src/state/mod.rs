// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! P-Chain on-disk state (`vms/platformvm/state`, specs 08 §3).
//!
//! Populated tier-by-tier across the M4 wave plan. The first landed piece is the
//! validator-metadata codec (§3.4): [`metadata_validator::ValidatorMetadata`]
//! and its three-version [`metadata_codec`].

pub mod l1_validator;
pub mod metadata_codec;
pub mod metadata_validator;
