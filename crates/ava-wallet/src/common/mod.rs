// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Chain-agnostic wallet helpers — port of `wallet/subnet/primary/common`
//! (`spend.go` owner matching + the deterministic UTXO ordering of
//! `common/utxotest`).

use std::collections::BTreeSet;

use ava_secp256k1fx::OutputOwners;
use ava_types::short_id::ShortId;

pub mod utxo_select;

/// `common.MatchOwners` — the sig indices (in owner-address order, up to
/// `threshold`) that `addrs` can sign for, or `None` if the owner is
/// timelocked past `min_issuance_time` or the threshold cannot be met.
#[must_use]
pub fn match_owners(
    owners: &OutputOwners,
    addrs: &BTreeSet<ShortId>,
    min_issuance_time: u64,
) -> Option<Vec<u32>> {
    if owners.locktime > min_issuance_time {
        return None;
    }

    let threshold = owners.threshold as usize;
    let mut sigs = Vec::with_capacity(threshold);
    for (i, addr) in owners.addrs.iter().enumerate() {
        if sigs.len() >= threshold {
            break;
        }
        if addrs.contains(addr) {
            // The index always fits: owner address lists are length-checked at
            // u32 by the codec.
            sigs.push(u32::try_from(i).unwrap_or(u32::MAX));
        }
    }
    (sigs.len() == threshold).then_some(sigs)
}
