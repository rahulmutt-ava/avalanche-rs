// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Deterministic UTXO selection helpers (specs 12 §13).
//!
//! Port of the P-chain builder's `splitByLocktime` / `splitByAssetID` /
//! `unwrapOutput` (`wallet/chain/p/builder/builder.go`) plus the canonical
//! UTXO ordering of `common/utxotest.DeterministicChainUTXOs` (sort by
//! `UTXOID.Compare`). The Go production backend iterates a map (random order);
//! the Rust port always sorts so the produced txs — and their ids — are
//! deterministic and byte-identical to the Go test vectors, which use the same
//! sorted order.

use std::cmp::Ordering;

use ava_platformvm::txs::components::Output;
use ava_platformvm::utxo::Utxo;
use ava_secp256k1fx::TransferOutput;

use crate::error::{Error, Result};

/// Canonical UTXO order — `UTXOID.Compare`: `(tx_id bytes, output_index)`.
#[must_use]
pub fn cmp_utxo_ids(a: &Utxo, b: &Utxo) -> Ordering {
    a.tx_id
        .to_bytes()
        .cmp(&b.tx_id.to_bytes())
        .then_with(|| a.output_index.cmp(&b.output_index))
}

/// Sorts `utxos` into the canonical (deterministic) selection order.
pub fn sort_utxos(utxos: &mut [Utxo]) {
    utxos.sort_by(cmp_utxo_ids);
}

/// `splitByLocktime` — `(unlocked, locked)` with respect to
/// `min_issuance_time`; preserves relative order within each half.
#[must_use]
pub fn split_by_locktime(utxos: Vec<Utxo>, min_issuance_time: u64) -> (Vec<Utxo>, Vec<Utxo>) {
    let mut unlocked = Vec::with_capacity(utxos.len());
    let mut locked = Vec::with_capacity(utxos.len());
    for utxo in utxos {
        match &utxo.out {
            Output::StakeableLock(lock) if min_issuance_time < lock.locktime => locked.push(utxo),
            _ => unlocked.push(utxo),
        }
    }
    (unlocked, locked)
}

/// `splitByAssetID` — `(requested, other)`; preserves relative order within
/// each half.
#[must_use]
pub fn split_by_asset_id(utxos: Vec<Utxo>, asset_id: ava_types::id::Id) -> (Vec<Utxo>, Vec<Utxo>) {
    let mut requested = Vec::with_capacity(utxos.len());
    let mut other = Vec::with_capacity(utxos.len());
    for utxo in utxos {
        if utxo.asset_id == asset_id {
            requested.push(utxo);
        } else {
            other.push(utxo);
        }
    }
    (requested, other)
}

/// `unwrapOutput` — the inner `secp256k1fx.TransferOutput` (and the stakeable
/// locktime, `0` if unlocked).
///
/// # Errors
/// [`Error::UnknownOutputType`] if the (possibly lock-wrapped) output is not a
/// `secp256k1fx.TransferOutput`.
pub fn unwrap_output(output: &Output) -> Result<(&TransferOutput, u64)> {
    match output {
        Output::Transfer(out) => Ok((out, 0)),
        Output::StakeableLock(lock) => match lock.transferable_out.as_ref() {
            Output::Transfer(out) => Ok((out, lock.locktime)),
            Output::StakeableLock(_) => Err(Error::UnknownOutputType),
        },
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::expect_used)]
mod tests {
    use ava_platformvm::stakeable::LockOut;
    use ava_secp256k1fx::OutputOwners;
    use ava_types::id::Id;
    use ava_types::short_id::ShortId;

    use super::*;

    fn utxo(tx_prefix: u64, output_index: u32, asset: Id, amt: u64, locktime: u64) -> Utxo {
        let inner = Output::Transfer(TransferOutput::new(
            amt,
            OutputOwners::new(0, 1, vec![ShortId::EMPTY]),
        ));
        Utxo {
            tx_id: Id::EMPTY.prefix(&[tx_prefix]),
            output_index,
            asset_id: asset,
            out: if locktime == 0 {
                inner
            } else {
                Output::StakeableLock(LockOut::new(locktime, inner))
            },
        }
    }

    /// Specs 12 §13: selection input order is canonical (sorted by UTXOID),
    /// locked UTXOs are preferred for staking before unlocked ones, locktimes
    /// are respected relative to the issuance time, and the whole pipeline is
    /// deterministic regardless of the backend's iteration order.
    #[test]
    fn deterministic_selection() {
        let avax = Id::EMPTY.prefix(&[1789]);
        let other_asset = Id::EMPTY.prefix(&[2024]);
        let now = 1_700_000_000u64;

        let canonical = vec![
            utxo(1, 1, avax, 2_000, 0),
            utxo(2, 2, avax, 3_000, now + 3_600), // locked
            utxo(3, 3, other_asset, 99, 0),
            utxo(4, 4, avax, 88_000, now + 3_600), // locked
            utxo(5, 5, avax, 9_000, 0),
        ];

        // Any permutation sorts to the same canonical order: the UTXOID
        // (tx_id bytes, output_index) order, independent of insertion order.
        let mut sorted_a = canonical.clone();
        sort_utxos(&mut sorted_a);
        let mut shuffled = vec![
            canonical[3].clone(),
            canonical[0].clone(),
            canonical[4].clone(),
            canonical[2].clone(),
            canonical[1].clone(),
        ];
        sort_utxos(&mut shuffled);
        assert_eq!(shuffled, sorted_a);
        assert!(
            sorted_a
                .windows(2)
                .all(|w| cmp_utxo_ids(&w[0], &w[1]) == Ordering::Less)
        );

        // Locked-vs-unlocked respects the issuance time and preserves the
        // canonical relative order within each half.
        let (unlocked, locked) = split_by_locktime(shuffled.clone(), now);
        let expect = |pred: fn(&Utxo) -> bool| -> Vec<u32> {
            sorted_a
                .iter()
                .filter(|u| pred(u))
                .map(|u| u.output_index)
                .collect()
        };
        assert_eq!(
            locked.iter().map(|u| u.output_index).collect::<Vec<_>>(),
            expect(|u| matches!(u.out, Output::StakeableLock(_)))
        );
        assert_eq!(
            unlocked.iter().map(|u| u.output_index).collect::<Vec<_>>(),
            expect(|u| matches!(u.out, Output::Transfer(_)))
        );
        // ...and a min-issuance-time at/after the locktime unlocks them.
        let (unlocked_later, locked_later) = split_by_locktime(shuffled.clone(), now + 3_600);
        assert_eq!(unlocked_later.len(), 5);
        assert!(locked_later.is_empty());

        // Fee-paying (AVAX) UTXOs are split out last, preserving order.
        let (requested, other) = split_by_asset_id(unlocked.clone(), avax);
        assert_eq!(
            requested.iter().map(|u| u.output_index).collect::<Vec<_>>(),
            unlocked
                .iter()
                .filter(|u| u.asset_id == avax)
                .map(|u| u.output_index)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            other.iter().map(|u| u.output_index).collect::<Vec<_>>(),
            vec![3]
        );

        // unwrap_output respects the stakeable wrapper.
        let (out, locktime) = unwrap_output(&canonical[1].out).expect("unwrap");
        assert_eq!(out.amt, 3_000);
        assert_eq!(locktime, now + 3_600);
        let (out, locktime) = unwrap_output(&canonical[0].out).expect("unwrap");
        assert_eq!(out.amt, 2_000);
        assert_eq!(locktime, 0);
    }
}
