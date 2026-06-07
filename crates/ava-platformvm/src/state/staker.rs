// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `state.Staker` — the ordering record for the current & pending validator
//! sets (`state/staker.go`, specs 08 §3.3).
//!
//! A [`Staker`] carries everything needed to represent a validator or delegator
//! in either set, and orders by the tuple `(next_time, priority, tx_id)` — the
//! Go `(*Staker).Less` btree comparator. `next_time` equals the `start_time`
//! while pending and the `end_time` while current, so a single ordering drives
//! time-advancement promotion (pending) and removal (current).
//!
//! The [`Ord`]/[`PartialOrd`]/[`Eq`]/[`PartialEq`] impls are all keyed on that
//! same ordering tuple so they stay mutually consistent (Rust requires `a == b`
//! iff `a.cmp(b) == Equal`). Go's full-field comparison lives separately on
//! [`Staker::equals`], mirroring `(*Staker).Equals`.

use std::cmp::Ordering;
use std::fmt;
use std::time::SystemTime;

use ava_crypto::bls;
use ava_types::id::Id;
use ava_types::node_id::NodeId;

use crate::txs::Priority;

/// `state.Staker` — a validator/delegator entry in the current or pending set.
///
/// Port of Go `state.Staker`. The struct is `Clone`/`Debug` but **not** a
/// blanket `PartialEq` derive: equality is defined by the ordering key (see the
/// module docs) and a separate [`Staker::equals`] reproduces Go's full-field
/// `Equals`. `Debug` is hand-written because [`bls::PublicKey`] is not `Debug`.
#[derive(Clone)]
pub struct Staker {
    /// `TxID` — the id of the tx that created this staker (the btree tie-break
    /// of last resort).
    pub tx_id: Id,
    /// `NodeID` — the validating node.
    pub node_id: NodeId,
    /// `PublicKey` — the BLS key; `None` for non-primary subnet stakers.
    pub public_key: Option<bls::PublicKey>,
    /// `SubnetID` — the subnet this staker validates (Primary Network is
    /// [`Id::EMPTY`]).
    pub subnet_id: Id,
    /// `Weight` — staking weight used when sampling.
    pub weight: u64,
    /// `StartTime` — when the staker begins validating.
    pub start_time: SystemTime,
    /// `EndTime` — when the staker stops validating.
    pub end_time: SystemTime,
    /// `PotentialReward` — the reward minted on a successful reward tx.
    pub potential_reward: u64,
    /// `NextTime` — the next time this staker moves between sets. Equals
    /// `start_time` while pending and `end_time` while current.
    pub next_time: SystemTime,
    /// `Priority` — breaks ties between stakers that share a `next_time`,
    /// grouping stakers created by the same tx type (`priorities.go`).
    pub priority: Priority,
}

impl Staker {
    /// `NewCurrentStaker` — a staker in the current validator set, where
    /// `next_time == end_time`. `priority` must be a current-set priority.
    ///
    /// Unlike the Go helper (which takes a `BoundedStaker` and derives the key /
    /// end time from the tx), this takes the resolved fields directly; the
    /// derivation of `priority`, `public_key`, etc. from the source tx is the
    /// caller's responsibility (executor wiring, later M4 task).
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new_current(
        tx_id: Id,
        node_id: NodeId,
        public_key: Option<bls::PublicKey>,
        subnet_id: Id,
        weight: u64,
        start_time: SystemTime,
        end_time: SystemTime,
        potential_reward: u64,
        priority: Priority,
    ) -> Self {
        Self {
            tx_id,
            node_id,
            public_key,
            subnet_id,
            weight,
            start_time,
            end_time,
            potential_reward,
            // Invariant: a current staker is next moved (removed) at its end.
            next_time: end_time,
            priority,
        }
    }

    /// `NewPendingStaker` — a staker in the pending validator set, where
    /// `next_time == start_time` and `potential_reward == 0` (the reward is
    /// assigned on promotion). `priority` must be a pending-set priority.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new_pending(
        tx_id: Id,
        node_id: NodeId,
        public_key: Option<bls::PublicKey>,
        subnet_id: Id,
        weight: u64,
        start_time: SystemTime,
        end_time: SystemTime,
        priority: Priority,
    ) -> Self {
        Self {
            tx_id,
            node_id,
            public_key,
            subnet_id,
            weight,
            start_time,
            end_time,
            potential_reward: 0,
            // Invariant: a pending staker is next moved (promoted) at its start.
            next_time: start_time,
            priority,
        }
    }

    /// The `(next_time, priority, tx_id)` ordering key — the basis of
    /// [`Ord`] and the equality impls. Mirrors `(*Staker).Less`.
    fn order_key(&self) -> (SystemTime, Priority, &Id) {
        (self.next_time, self.priority, &self.tx_id)
    }

    /// `(*Staker).Equals` — full-field equality, distinct from the ordering-key
    /// [`PartialEq`]. Two `None` keys are equal; two `Some` keys compare by
    /// their compressed bytes (`bls.PublicKey.Equals`).
    #[must_use]
    pub fn equals(&self, other: &Self) -> bool {
        let equal_pks = match (&self.public_key, &other.public_key) {
            (None, None) => true,
            (Some(a), Some(b)) => a.compress() == b.compress(),
            _ => false,
        };
        self.tx_id == other.tx_id
            && self.node_id == other.node_id
            && equal_pks
            && self.subnet_id == other.subnet_id
            && self.weight == other.weight
            && self.start_time == other.start_time
            && self.end_time == other.end_time
            && self.potential_reward == other.potential_reward
            && self.next_time == other.next_time
            && self.priority == other.priority
    }
}

impl fmt::Debug for Staker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Staker")
            .field("tx_id", &self.tx_id)
            .field("node_id", &self.node_id)
            // bls::PublicKey is not Debug; show its compressed bytes (or None).
            .field(
                "public_key",
                &self.public_key.as_ref().map(bls::PublicKey::compress),
            )
            .field("subnet_id", &self.subnet_id)
            .field("weight", &self.weight)
            .field("start_time", &self.start_time)
            .field("end_time", &self.end_time)
            .field("potential_reward", &self.potential_reward)
            .field("next_time", &self.next_time)
            .field("priority", &self.priority)
            .finish()
    }
}

impl Ord for Staker {
    /// `(*Staker).Less` — order by `next_time`, then `priority` (lower first),
    /// then `tx_id` bytes (`bytes.Compare`; [`Id`] derives lexicographic
    /// [`Ord`]).
    fn cmp(&self, other: &Self) -> Ordering {
        self.next_time
            .cmp(&other.next_time)
            .then(self.priority.cmp(&other.priority))
            .then(self.tx_id.cmp(&other.tx_id))
    }
}

impl PartialOrd for Staker {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Staker {
    /// Equality on the ordering key, kept consistent with [`Ord`]. Use
    /// [`Staker::equals`] for Go's full-field `Equals`.
    fn eq(&self, other: &Self) -> bool {
        self.order_key() == other.order_key()
    }
}

impl Eq for Staker {}

#[cfg(test)]
mod prop {
    //! `staker_ord_matches_go` — the ordering is a total order keyed on
    //! `(next_time, priority, tx_id)`, mirroring Go `(*Staker).Less`.

    use std::time::{Duration, UNIX_EPOCH};

    use proptest::prelude::*;

    use super::*;

    /// All 11 priorities, in discriminant order, for indexed selection.
    const ALL_PRIORITIES: [Priority; 11] = [
        Priority::PrimaryNetworkDelegatorApricotPending,
        Priority::PrimaryNetworkValidatorPending,
        Priority::PrimaryNetworkDelegatorBanffPending,
        Priority::SubnetPermissionlessValidatorPending,
        Priority::SubnetPermissionlessDelegatorPending,
        Priority::SubnetPermissionedValidatorPending,
        Priority::SubnetPermissionedValidatorCurrent,
        Priority::SubnetPermissionlessDelegatorCurrent,
        Priority::SubnetPermissionlessValidatorCurrent,
        Priority::PrimaryNetworkDelegatorCurrent,
        Priority::PrimaryNetworkValidatorCurrent,
    ];

    /// A small-domain staker strategy: the ordering only inspects
    /// `(next_time, priority, tx_id)`, and the domains are deliberately tiny so
    /// ties (equal keys) are exercised frequently.
    fn staker_strategy() -> impl Strategy<Value = Staker> {
        // next_time over a few-second window, a sampled priority, and a 32-byte
        // tx_id whose bytes are drawn from a small set (frequent collisions).
        (
            0u64..4,
            proptest::sample::select(ALL_PRIORITIES.as_slice()),
            proptest::array::uniform32(0u8..4),
        )
            .prop_map(|(secs, priority, tx_bytes)| {
                let next_time = UNIX_EPOCH
                    .checked_add(Duration::from_secs(secs))
                    .expect("within SystemTime range");
                Staker {
                    tx_id: Id::from(tx_bytes),
                    node_id: NodeId::EMPTY,
                    public_key: None,
                    subnet_id: Id::EMPTY,
                    weight: 0,
                    start_time: next_time,
                    end_time: next_time,
                    potential_reward: 0,
                    next_time,
                    priority,
                }
            })
    }

    /// The reference key Go's `Less` compares on, as a `Rust`-orderable tuple.
    fn go_key(s: &Staker) -> (SystemTime, u8, [u8; 32]) {
        (s.next_time, s.priority.as_u8(), *s.tx_id.as_bytes())
    }

    proptest! {
        #[test]
        fn staker_ord_matches_go(a in staker_strategy(), b in staker_strategy()) {
            // Staker::cmp must equal the tuple-key comparison exactly.
            prop_assert_eq!(a.cmp(&b), go_key(&a).cmp(&go_key(&b)));
            // Eq is consistent with Ord.
            prop_assert_eq!(a == b, a.cmp(&b) == Ordering::Equal);
        }

        #[test]
        fn staker_ord_is_total_order(
            a in staker_strategy(),
            b in staker_strategy(),
            c in staker_strategy(),
        ) {
            // Antisymmetry: a<=b and b<=a implies a==b (on the ordering key).
            if a <= b && b <= a {
                prop_assert_eq!(a.cmp(&b), Ordering::Equal);
            }
            // Transitivity: a<=b and b<=c implies a<=c.
            if a <= b && b <= c {
                prop_assert!(a <= c);
            }
            // Totality: exactly one of <, ==, > holds.
            let ord = a.cmp(&b);
            prop_assert_eq!(ord.reverse(), b.cmp(&a));
        }
    }
}
