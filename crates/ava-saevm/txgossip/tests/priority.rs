// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Effective-tip priority ordering + `Set` invariants + gossip RLP round-trip
//! (specs/11 §9.2; `02` §4 mempool invariant).

use ava_evm_reth::{Decodable2718, TransactionSigned};
use ava_saevm_txgossip::priority::Priced;
use ava_saevm_txgossip::{Gossipable, Set, Transaction, TransactionsByPriority, TxMarshaller};
use pretty_assertions::assert_eq;
use proptest::prelude::*;

/// A synthetic priced tx for exercising the ordering without signing real txs:
/// it reports a fixed effective tip at any base fee (the `max_priority`-style
/// tip, saturating-subtracting the base fee) plus a nonce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FakeTx {
    /// `max_fee_per_gas`-style cap (per gas).
    cap: u128,
    nonce: u64,
    /// A unique tag so we can assert identity in the pop order.
    tag: u32,
}

impl Priced for FakeTx {
    fn effective_tip(&self, base_fee: u64) -> Option<u128> {
        // Mirrors EIP-1559 `effective_tip`: `None` if it cannot pay base_fee.
        let base = u128::from(base_fee);
        if self.cap < base {
            return None;
        }
        Some(self.cap - base)
    }

    fn nonce(&self) -> u64 {
        self.nonce
    }
}

// ---------------------------------------------------------------------------
// table test: orders by effective tip at a fixed base fee
// ---------------------------------------------------------------------------

#[test]
fn transactions_by_priority_orders_by_effective_tip() {
    let base_fee = 100u64;
    // cap -> effective tip at base 100: 350->250, 200->100, 100->0, 90->skip.
    let txs = vec![
        FakeTx {
            cap: 200,
            nonce: 5,
            tag: 1,
        }, // tip 100
        FakeTx {
            cap: 90,
            nonce: 0,
            tag: 2,
        }, // tip None (skipped)
        FakeTx {
            cap: 350,
            nonce: 9,
            tag: 3,
        }, // tip 250
        FakeTx {
            cap: 100,
            nonce: 1,
            tag: 4,
        }, // tip 0
    ];
    let mut p = TransactionsByPriority::new(txs, base_fee);
    let order: Vec<u32> = std::iter::from_fn(|| p.pop().map(|t| t.tag)).collect();
    // Highest tip first; cap=90 dropped (cannot pay base fee).
    assert_eq!(order, vec![3, 1, 4]);
}

#[test]
fn ties_broken_by_nonce_then_arrival() {
    let base_fee = 0u64;
    // All same cap => same tip; order must be nonce asc, then arrival asc.
    let txs = vec![
        FakeTx {
            cap: 10,
            nonce: 2,
            tag: 10,
        }, // arrival 0
        FakeTx {
            cap: 10,
            nonce: 1,
            tag: 11,
        }, // arrival 1
        FakeTx {
            cap: 10,
            nonce: 1,
            tag: 12,
        }, // arrival 2 (same nonce as 11)
        FakeTx {
            cap: 10,
            nonce: 0,
            tag: 13,
        }, // arrival 3
    ];
    let p = TransactionsByPriority::new(txs, base_fee);
    let order: Vec<u32> = p.as_slice().iter().map(|t| t.tag).collect();
    // nonce 0 (13), nonce 1 fifo (11 then 12), nonce 2 (10).
    assert_eq!(order, vec![13, 11, 12, 10]);
}

// ---------------------------------------------------------------------------
// proptest: pop order is a strict total order by (tip desc, nonce asc, arrival)
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn pop_order_total_order_by_tip_nonce_arrival(
        caps in proptest::collection::vec(0u128..1000, 0..40),
        nonces in proptest::collection::vec(0u64..50, 0..40),
        base_fee in 0u64..500,
    ) {
        let n = caps.len().min(nonces.len());
        let txs: Vec<FakeTx> = (0..n)
            .map(|i| FakeTx { cap: caps[i], nonce: nonces[i], tag: u32::try_from(i).unwrap() })
            .collect();

        // Expected: filter eligible, then stable-sort by the documented key.
        let mut expected: Vec<(usize, FakeTx)> = txs
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, t)| t.effective_tip(base_fee).is_some())
            .collect();
        expected.sort_by(|(ia, a), (ib, b)| {
            b.effective_tip(base_fee)
                .cmp(&a.effective_tip(base_fee))
                .then_with(|| a.nonce.cmp(&b.nonce))
                .then_with(|| ia.cmp(ib))
        });
        let expected_tags: Vec<u32> = expected.iter().map(|(_, t)| t.tag).collect();

        let p = TransactionsByPriority::new(txs, base_fee);
        let got_tags: Vec<u32> = p.as_slice().iter().map(|t| t.tag).collect();
        prop_assert_eq!(&got_tags, &expected_tags);

        // Adjacent-pair monotonicity: tip non-increasing; ties => nonce/arrival.
        let order = p.into_ordered();
        for w in order.windows(2) {
            let a = w[0];
            let b = w[1];
            let ta = a.effective_tip(base_fee).unwrap();
            let tb = b.effective_tip(base_fee).unwrap();
            prop_assert!(ta >= tb);
            if ta == tb {
                prop_assert!(a.nonce <= b.nonce);
            }
        }
        // No eligible tx lost / none invented.
        prop_assert_eq!(got_tags.len(), expected_tags.len());
    }
}

// ---------------------------------------------------------------------------
// Set invariants — built on real txs (decoded fixtures)
// ---------------------------------------------------------------------------

/// A recorded legacy signed tx (the inner tx of the shared `block_wire` vector:
/// nonce 0, gasPrice 0x34630b8a00, gas 21000, to 0x2222…, value 1e18). It is a
/// valid EIP-2718 (legacy) envelope, so `Decodable2718` decodes it and the
/// signature recovers a sender.
const RAW_LEGACY_TX: &str = "f86c808534630b8a00825208942222222222222222222222222222222222222222880de0b6b3a76400008025a00955b36c6d8fa1d97e5afe2d398abac0eb7b6e99858114ad14efc90defc96b0ba005e1f1c07f78c649c01f51824ec54dd817acbea818eb242511d641903d4d5b94";

fn fixture_tx() -> Transaction {
    let bytes = hex::decode(RAW_LEGACY_TX).expect("tx hex");
    let signed = TransactionSigned::decode_2718(&mut bytes.as_slice()).expect("decode 2718");
    let recovered =
        ava_evm_reth::SignerRecoverable::try_into_recovered(signed).expect("recover sender");
    Transaction::new(recovered)
}

#[test]
fn add_remove_idempotent() {
    let mut set = Set::new();
    let tx = fixture_tx();
    let id = tx.gossip_id();

    assert!(set.add(tx.clone()));
    assert_eq!(set.len(), 1);
    // Double-add is a no-op.
    assert!(!set.add(tx.clone()));
    assert_eq!(set.len(), 1);

    assert!(set.remove(&id));
    assert_eq!(set.len(), 0);
    // Double-remove is a no-op.
    assert!(!set.remove(&id));
    assert_eq!(set.len(), 0);
}

#[test]
fn no_tx_lost() {
    let mut set = Set::new();
    let tx = fixture_tx();
    let id = tx.gossip_id();

    set.add(tx);
    // Every added tx is either pending or explicitly removed — never lost.
    assert!(set.contains(&id));
    assert_eq!(set.pending().len(), 1);

    set.remove(&id);
    assert!(!set.contains(&id));
    // Still recorded as seen (so stale gossip cannot silently re-add it).
    assert!(set.seen(&id));
}

#[test]
fn gossipable_rlp_roundtrip() {
    let m = TxMarshaller::new();
    let tx = fixture_tx();
    let bytes = m.marshal(&tx);
    let tx2 = m.unmarshal(&bytes).expect("unmarshal");
    assert_eq!(tx2.gossip_id(), tx.gossip_id());
}
