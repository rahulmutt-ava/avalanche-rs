// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Codec round-trip, a frozen golden blob, and the height-indexed results DB
//! for `ExecutionResults` (specs/11 §4.1/§7).

use ava_database::HeightIndexMemDb;
use ava_evm_reth::B256;
use ava_saevm_proxytime::Time;
use ava_saevm_types::{EXECUTION_RESULTS_LEN, ExecutionResults, ExecutionResultsDb};
use ava_vm::components::gas::Price;
use proptest::prelude::*;

fn b256(seed: u8) -> B256 {
    B256::from([seed; 32])
}

#[test]
fn execution_results_golden() {
    // A fixed, known ExecutionResults encodes to a deterministic 96-byte blob.
    let results = ExecutionResults {
        gas_time: Time::new(0x0102_0304_0506_0708, 0x1112_1314, 0x2122_2324),
        base_fee: Price(0xA1A2_A3A4_A5A6_A7A8),
        receipt_root: b256(0xBB),
        post_state_root: b256(0xCC),
    };

    let blob = results.encode();
    assert_eq!(blob.len(), EXECUTION_RESULTS_LEN);
    assert_eq!(blob.len(), 96);

    // gas_time: 24 bytes big-endian [seconds, fraction, hertz].
    let mut want = Vec::with_capacity(96);
    want.extend_from_slice(&0x0102_0304_0506_0708u64.to_be_bytes());
    want.extend_from_slice(&0x1112_1314u64.to_be_bytes());
    want.extend_from_slice(&0x2122_2324u64.to_be_bytes());
    // base_fee: 8 bytes big-endian.
    want.extend_from_slice(&0xA1A2_A3A4_A5A6_A7A8u64.to_be_bytes());
    // receipt_root, post_state_root: 32 bytes each.
    want.extend_from_slice(&[0xBB; 32]);
    want.extend_from_slice(&[0xCC; 32]);

    assert_eq!(blob, want);

    // And it round-trips.
    let decoded = ExecutionResults::decode(&blob).expect("decode");
    assert_eq!(decoded, results);
}

#[test]
fn decode_rejects_wrong_length() {
    assert!(ExecutionResults::decode(&[0u8; 95]).is_err());
    assert!(ExecutionResults::decode(&[0u8; 97]).is_err());
    assert!(ExecutionResults::decode(&[]).is_err());
}

#[test]
fn height_index_get_put() {
    let db = ExecutionResultsDb::new(HeightIndexMemDb::new());
    let results = ExecutionResults {
        gas_time: Time::new(42, 7, 100),
        base_fee: Price(1_000),
        receipt_root: b256(0x01),
        post_state_root: b256(0x02),
    };

    assert!(!db.has(9).expect("has"));
    db.put(9, &results).expect("put");
    assert!(db.has(9).expect("has"));

    let got = db.get(9).expect("get");
    assert_eq!(got, results);

    // A missing height is an error (NotFound), not a panic.
    assert!(db.get(10).is_err());
}

proptest! {
    /// `decode(encode(x)) == x` for arbitrary results.
    #[test]
    fn execution_results_roundtrip(
        seconds in any::<u64>(),
        hertz in 1u64..=u64::MAX,
        base_fee in any::<u64>(),
        rr in any::<[u8; 32]>(),
        psr in any::<[u8; 32]>(),
        frac_raw in any::<u64>(),
    ) {
        // fraction must be < hertz; Time::new normalises but keep it simple.
        // checked_rem (not `%`) keeps the SAE arithmetic_side_effects bar happy.
        let fraction = frac_raw.checked_rem(hertz).unwrap_or(0);
        let results = ExecutionResults {
            gas_time: Time::new(seconds, fraction, hertz),
            base_fee: Price(base_fee),
            receipt_root: B256::from(rr),
            post_state_root: B256::from(psr),
        };
        let blob = results.encode();
        prop_assert_eq!(blob.len(), EXECUTION_RESULTS_LEN);
        let decoded = ExecutionResults::decode(&blob).expect("decode");
        prop_assert_eq!(decoded, results);
    }
}
