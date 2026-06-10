// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `genesis.go::splitAllocations` — the greedy split of the initially-staked
//! allocations across the initial stakers (specs 23 §3.3.1).
//!
//! The loop is reproduced **verbatim** from Go: the exact split determines each
//! genesis validator's stake outputs and weight, hence the validator tx IDs and
//! the P-Chain genesis bytes.

use crate::config::{Allocation, LockedAmount};

/// Splits `allocations` into `num_splits` buckets of roughly
/// `node_weight = total_staked_amount / num_splits` each (integer division;
/// the remainder lands in the last bucket).
///
/// Walks allocations in order; within each, walks the unlock schedule in
/// order; carves the current bucket's unlock entries until it reaches
/// `node_weight`, splitting an `unlock.amount` across the bucket boundary when
/// needed. Each emitted sub-allocation has `initial_amount = 0` and a freshly
/// built unlock schedule. Arithmetic wraps like Go's `uint64` (`+=` /
/// `totalAmount` are unchecked in `splitAllocations`).
///
/// `num_splits` must be `> 0` (the caller validates `NoStakers` first); a zero
/// value returns no buckets instead of Go's divide-by-zero panic.
#[must_use]
pub fn split_allocations(allocations: &[Allocation], num_splits: usize) -> Vec<Vec<Allocation>> {
    if num_splits == 0 {
        return Vec::new();
    }
    let mut total_amount: u64 = 0;
    for allocation in allocations {
        for unlock in &allocation.unlock_schedule {
            total_amount = total_amount.wrapping_add(unlock.amount);
        }
    }

    // num_splits > 0 is guaranteed above; checked_div only for the lint.
    let node_weight = total_amount.checked_div(num_splits as u64).unwrap_or(0);
    let mut all_node_allocations: Vec<Vec<Allocation>> = Vec::with_capacity(num_splits);

    /// `currentAllocation := allocation; .InitialAmount = 0; .UnlockSchedule = nil`.
    fn fresh(allocation: &Allocation) -> Allocation {
        Allocation {
            eth_addr: allocation.eth_addr,
            avax_addr: allocation.avax_addr,
            initial_amount: 0,
            unlock_schedule: Vec::new(),
        }
    }

    let mut current_node_allocation: Vec<Allocation> = Vec::new();
    let mut current_node_amount: u64 = 0;
    for allocation in allocations {
        let mut current_allocation = fresh(allocation);

        for unlock in &allocation.unlock_schedule {
            let mut unlock = *unlock; // Go iterates by value; `unlock.Amount` is mutated.
            while current_node_amount.wrapping_add(unlock.amount) > node_weight
                && all_node_allocations.len() < num_splits.saturating_sub(1)
            {
                let amount_to_add = node_weight.wrapping_sub(current_node_amount);
                current_allocation.unlock_schedule.push(LockedAmount {
                    amount: amount_to_add,
                    locktime: unlock.locktime,
                });
                unlock.amount = unlock.amount.wrapping_sub(amount_to_add);

                current_node_allocation.push(current_allocation);
                all_node_allocations.push(std::mem::take(&mut current_node_allocation));
                current_node_amount = 0;

                current_allocation = fresh(allocation);
            }

            if unlock.amount == 0 {
                continue;
            }

            current_allocation.unlock_schedule.push(LockedAmount {
                amount: unlock.amount,
                locktime: unlock.locktime,
            });
            current_node_amount = current_node_amount.wrapping_add(unlock.amount);
        }

        if !current_allocation.unlock_schedule.is_empty() {
            current_node_allocation.push(current_allocation);
        }
    }

    all_node_allocations.push(current_node_allocation);
    all_node_allocations
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)] // fixed-size vectors asserted by length first
mod tests {
    use ava_types::short_id::ShortId;

    use super::*;

    fn alloc(tag: u8, schedule: &[(u64, u64)]) -> Allocation {
        Allocation {
            eth_addr: ShortId::from([tag; 20]),
            avax_addr: ShortId::from([tag; 20]),
            initial_amount: 0,
            unlock_schedule: schedule
                .iter()
                .map(|&(amount, locktime)| LockedAmount { amount, locktime })
                .collect(),
        }
    }

    fn bucket_weight(bucket: &[Allocation]) -> u64 {
        bucket
            .iter()
            .flat_map(|a| &a.unlock_schedule)
            .map(|u| u.amount)
            .sum()
    }

    /// M8.6 red test: fixed staked-allocation sets × `num_splits` — per-bucket
    /// unlock schedules + weights reproduce the Go loop (specs 23 §3.3.1/§9.4).
    #[test]
    fn split_allocations_vectors() {
        // Vector 1: total 25, 2 splits → node_weight 12; the 10@t2 entry is
        // carved 2/8 across the bucket boundary; remainder (13) in the last.
        let allocations = [alloc(0xaa, &[(10, 1), (10, 2)]), alloc(0xbb, &[(5, 3)])];
        let got = split_allocations(&allocations, 2);
        assert_eq!(got.len(), 2);
        assert_eq!(bucket_weight(&got[0]), 12);
        assert_eq!(bucket_weight(&got[1]), 13);
        assert_eq!(
            got[0],
            vec![alloc(0xaa, &[(10, 1), (2, 2)])],
            "bucket 0 carves the second unlock at the boundary"
        );
        assert_eq!(
            got[1],
            vec![alloc(0xaa, &[(8, 2)]), alloc(0xbb, &[(5, 3)])],
            "bucket 1 takes the carved remainder then the next allocation"
        );
        // initial_amount is always zeroed in the emitted sub-allocations.
        assert!(got.iter().flatten().all(|a| a.initial_amount == 0));

        // Vector 2 (exact-fill edge): total 20, 2 splits → node_weight 10. Go
        // emits a zero-amount boundary entry for 0xbb in bucket 0 — reproduce it.
        let allocations = [alloc(0xaa, &[(10, 1)]), alloc(0xbb, &[(10, 2)])];
        let got = split_allocations(&allocations, 2);
        assert_eq!(got.len(), 2);
        assert_eq!(
            got[0],
            vec![alloc(0xaa, &[(10, 1)]), alloc(0xbb, &[(0, 2)])],
            "bucket 0 includes Go's zero-amount boundary carve"
        );
        assert_eq!(got[1], vec![alloc(0xbb, &[(10, 2)])]);

        // Vector 3: one staker takes everything in config order, unsplit.
        let allocations = [alloc(0xaa, &[(7, 9)]), alloc(0xbb, &[(3, 1)])];
        let got = split_allocations(&allocations, 1);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0], vec![alloc(0xaa, &[(7, 9)]), alloc(0xbb, &[(3, 1)])]);
    }
}
