// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M4.13 TDD entry tests for the `Chain`/`Diff`/`Versions`/`State` flat-KV
//! stores (`vms/platformvm/state`, specs 08 §3).
//!
//! - [`prop::diff_apply_equals_direct`] — the overlay-flush oracle: a sequence of
//!   stat mutations applied through a [`Diff`] then `apply()`'d to a base
//!   [`State`] equals applying them directly to the [`State`].
//! - [`conformance::state_roundtrip`] — write then re-read UTXOs / stakers /
//!   supply across a RocksDB temp dir (and a `MemDb` mirror).

// This suite exercises only the state subsystem; the dev-deps declared for the
// codec/reward suites are unused here.
#![allow(unused_crate_dependencies)]
// Integration-test helpers (not `#[test]`-attributed) are not recognized by
// clippy's allow-{expect,unwrap}-in-tests heuristic, and the fingerprint tuple is
// deliberately wide; these are test-only and harmless here.
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::arithmetic_side_effects, clippy::type_complexity)]

use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use ava_database::{MemDb, RocksDb};
use ava_platformvm::state::chain::{Chain, Versions};
use ava_platformvm::state::diff::Diff;
use ava_platformvm::state::staker::Staker;
use ava_platformvm::state::state::State;
use ava_platformvm::txs::Priority;
use ava_platformvm::txs::fee::gas::GasState;
use ava_types::id::Id;
use ava_types::node_id::NodeId;

/// A `Versions` that resolves exactly one parent block ID to a shared `Chain`.
struct SingleParent {
    id: Id,
    chain: Arc<dyn Chain>,
}

impl Versions for SingleParent {
    fn get_state(&self, block_id: Id) -> Option<Arc<dyn Chain>> {
        if block_id == self.id {
            Some(Arc::clone(&self.chain))
        } else {
            None
        }
    }
}

fn pending_validator(seed: u8, subnet: Id, node: NodeId) -> Staker {
    let start = UNIX_EPOCH + Duration::from_secs(u64::from(seed) + 100);
    let end = start + Duration::from_secs(1_000);
    Staker::new_pending(
        Id::from([seed; 32]),
        node,
        None,
        subnet,
        u64::from(seed) + 1,
        start,
        end,
        Priority::SubnetPermissionlessValidatorPending,
    )
}

mod conformance {
    use super::*;

    /// Write UTXOs / stakers / supply to a `State` over a RocksDB temp dir, then
    /// re-read them (a fresh `State` over the same dir would require a commit/
    /// reopen path; here we assert the in-memory `Chain` surface round-trips and
    /// at least one case is RocksDb-backed per the plan).
    #[test]
    fn state_roundtrip() {
        let rocks = RocksDb::open_temp().expect("open rocksdb temp dir");
        roundtrip_over(State::new(rocks).expect("state over rocksdb"));

        // MemDb mirror — same assertions, no temp dir required.
        roundtrip_over(State::new(MemDb::new()).expect("state over memdb"));
    }

    fn roundtrip_over<C: Chain>(mut state: C) {
        let subnet = Id::from([7u8; 32]);
        let node = NodeId::from([3u8; 20]);

        // supply
        state.set_current_supply(Id::EMPTY, 1_000_000);
        state.set_current_supply(subnet, 42);
        assert_eq!(
            state.current_supply(Id::EMPTY).expect("primary supply"),
            1_000_000
        );
        assert_eq!(state.current_supply(subnet).expect("subnet supply"), 42);

        // timestamp / fee state
        let ts = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        state.set_timestamp(ts);
        assert_eq!(state.timestamp(), ts);
        let gas = GasState {
            capacity: 123,
            excess: 456,
        };
        state.set_fee_state(gas);
        assert_eq!(state.fee_state(), gas);
        state.set_accrued_fees(99);
        assert_eq!(state.accrued_fees(), 99);

        // UTXOs (opaque bytes)
        let utxo_id = Id::from([9u8; 32]);
        state.add_utxo(utxo_id, vec![1, 2, 3, 4]);
        assert_eq!(state.get_utxo(utxo_id).expect("utxo"), vec![1, 2, 3, 4]);
        state.delete_utxo(utxo_id);
        assert!(state.get_utxo(utxo_id).is_err());

        // reward UTXOs keyed by staker tx id
        let staker_tx = Id::from([5u8; 32]);
        state.add_reward_utxo(staker_tx, vec![10, 11]);
        state.add_reward_utxo(staker_tx, vec![12]);
        assert_eq!(
            state.get_reward_utxos(staker_tx),
            vec![vec![10, 11], vec![12]]
        );

        // stakers
        let s = pending_validator(1, subnet, node);
        state.put_pending_validator(s.clone()).expect("put pending");
        assert_eq!(state.pending_stakers().len(), 1);
        let cur = Staker::new_current(
            s.tx_id,
            node,
            None,
            subnet,
            s.weight,
            s.start_time,
            s.end_time,
            7,
            Priority::SubnetPermissionlessValidatorCurrent,
        );
        state
            .put_current_validator(cur.clone())
            .expect("put current");
        let got = state
            .get_current_validator(subnet, node)
            .expect("get current");
        assert!(got.equals(&cur));
        state.delete_pending_validator(&s);
        assert!(state.pending_stakers().is_empty());
    }
}

mod prop {
    use proptest::prelude::*;

    use super::*;

    /// A single stat mutation applied to any `Chain`.
    #[derive(Clone, Debug)]
    enum Mutation {
        Timestamp(u64),
        PrimarySupply(u64),
        SubnetSupply(u8, u64),
        FeeState(u64, u64),
        AccruedFees(u64),
        L1Excess(u64),
        AddUtxo(u8, Vec<u8>),
        DeleteUtxo(u8),
        PutPendingValidator(u8),
        DeletePendingValidator(u8),
    }

    fn apply(c: &mut dyn Chain, m: &Mutation) {
        match m {
            Mutation::Timestamp(s) => c.set_timestamp(UNIX_EPOCH + Duration::from_secs(*s)),
            Mutation::PrimarySupply(v) => c.set_current_supply(Id::EMPTY, *v),
            Mutation::SubnetSupply(id, v) => c.set_current_supply(Id::from([*id; 32]), *v),
            Mutation::FeeState(cap, ex) => c.set_fee_state(GasState {
                capacity: *cap,
                excess: *ex,
            }),
            Mutation::AccruedFees(v) => c.set_accrued_fees(*v),
            Mutation::L1Excess(v) => c.set_l1_validator_excess(*v),
            Mutation::AddUtxo(id, b) => c.add_utxo(Id::from([*id; 32]), b.clone()),
            Mutation::DeleteUtxo(id) => c.delete_utxo(Id::from([*id; 32])),
            Mutation::PutPendingValidator(seed) => {
                let _ = c.put_pending_validator(pending_validator(
                    *seed,
                    Id::from([*seed; 32]),
                    NodeId::from([*seed; 20]),
                ));
            }
            Mutation::DeletePendingValidator(seed) => {
                c.delete_pending_validator(&pending_validator(
                    *seed,
                    Id::from([*seed; 32]),
                    NodeId::from([*seed; 20]),
                ));
            }
        }
    }

    /// Observable scalar/aggregate fingerprint of a `Chain`, for equality.
    fn fingerprint(
        c: &dyn Chain,
    ) -> (
        std::time::SystemTime,
        u64,
        u64,
        GasState,
        u64,
        u64,
        Vec<(Vec<u8>, Vec<u8>)>,
        usize,
    ) {
        let mut utxos = Vec::new();
        for id in 0u8..16 {
            let key = Id::from([id; 32]);
            if let Ok(b) = c.get_utxo(key) {
                utxos.push((key.to_bytes().to_vec(), b));
            }
        }
        (
            c.timestamp(),
            c.current_supply(Id::EMPTY).unwrap_or(0),
            c.current_supply(Id::from([1u8; 32])).unwrap_or(0),
            c.fee_state(),
            c.accrued_fees(),
            c.l1_validator_excess(),
            utxos,
            c.pending_stakers().len(),
        )
    }

    fn mutation_strategy() -> impl Strategy<Value = Mutation> {
        prop_oneof![
            (0u64..3).prop_map(Mutation::Timestamp),
            any::<u64>().prop_map(Mutation::PrimarySupply),
            (0u8..3, any::<u64>()).prop_map(|(i, v)| Mutation::SubnetSupply(i, v)),
            (any::<u64>(), any::<u64>()).prop_map(|(a, b)| Mutation::FeeState(a, b)),
            any::<u64>().prop_map(Mutation::AccruedFees),
            any::<u64>().prop_map(Mutation::L1Excess),
            (0u8..4, proptest::collection::vec(any::<u8>(), 0..4))
                .prop_map(|(i, b)| Mutation::AddUtxo(i, b)),
            (0u8..4).prop_map(Mutation::DeleteUtxo),
            (0u8..4).prop_map(Mutation::PutPendingValidator),
            (0u8..4).prop_map(Mutation::DeletePendingValidator),
        ]
    }

    proptest! {
        #[test]
        fn diff_apply_equals_direct(muts in proptest::collection::vec(mutation_strategy(), 0..24)) {
            // Direct: apply every mutation straight to a base State.
            let mut direct = State::new(MemDb::new()).expect("direct state");
            for m in &muts {
                apply(&mut direct, m);
            }

            // Overlay: apply through a Diff over a fresh base, then flush.
            let parent_id = Id::from([0xABu8; 32]);
            let base: Arc<dyn Chain> = Arc::new(State::new(MemDb::new()).expect("base state"));
            let versions = SingleParent { id: parent_id, chain: base };
            let mut diff = Diff::new(parent_id, &versions).expect("diff over parent");
            for m in &muts {
                apply(&mut diff, m);
            }
            let mut flushed = State::new(MemDb::new()).expect("flush target");
            diff.apply(&mut flushed).expect("flush diff");

            prop_assert_eq!(fingerprint(&direct), fingerprint(&flushed));
        }
    }
}
