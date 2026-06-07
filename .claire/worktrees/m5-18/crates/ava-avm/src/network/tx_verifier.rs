// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! X-Chain tx verifier seam for the gossip handler (specs 09 §8;
//! `vms/avm/network/tx_verifier.go`).
//!
//! Mirrors the P-Chain precedent in
//! `crates/ava-platformvm/src/network.rs`.

use ava_types::id::Id;

use crate::txs::Tx;
