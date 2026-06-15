// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Fuzz target: decode-never-panics over arbitrary bytes for the C-Chain block
//! decoder (`specs/10` §9.3 / §6.2, M6.7/M6.28).
//!
//! Drives `block::decode_ava_evm_block` with arbitrary input, asserting only
//! that decoding never panics — errors are expected and ignored. The spec is
//! constructed with all Avalanche phases active from genesis (matching the
//! `block_wire` integration test's "all_active" spec), covering both plain and
//! atomic-block code paths at once.
//!
//! Where decoding succeeds, a round-trip is asserted: `assemble_ava_block`
//! re-encodes the decoded parts and the resulting bytes must be byte-identical
//! to the input (the canonical coreth invariant: `parse → assemble` is
//! identity).

#![no_main]

use libfuzzer_sys::fuzz_target;

use ava_evm::block::{assemble_ava_block, decode_ava_evm_block};
use ava_evm::chainspec::{AvaChainSpec, NetworkUpgrades};
use ava_evm_reth::Chain;

/// A spec with all Avalanche phases active from genesis (timestamps all at 0),
/// matching the `block_wire` integration test's `all_active` setup. Using a
/// constant-initialized static avoids rebuilding the spec per fuzz iteration.
fn all_active_spec() -> AvaChainSpec {
    // All phases active from t=0: covers both pre-AP5 (single-tx ext_data)
    // and AP5+ (batch ext_data) decode paths regardless of block timestamp.
    let all_active = NetworkUpgrades {
        apricot_phase_1: 0,
        apricot_phase_2: 0,
        apricot_phase_3: 0,
        apricot_phase_4: 0,
        apricot_phase_5: 0,
        apricot_phase_pre_6: 0,
        apricot_phase_6: 0,
        apricot_phase_post_6: 0,
        banff: 0,
        cortina: 0,
        durango: 0,
        etna: 0,
        fortuna: 0,
        granite: 0,
        helicon: u64::MAX,
    };
    AvaChainSpec::from_parts(all_active, Chain::from_id(43114), false)
}

fuzz_target!(|data: &[u8]| {
    let spec = all_active_spec();

    // Decode-never-panics: arbitrary bytes must not cause a panic, only errors.
    if let Ok(block) = decode_ava_evm_block(data, &spec) {
        // Round-trip stability (spec 10 §9.3 / §6.2): re-assembling the decoded
        // parts must produce byte-identical output.
        let parts = block.into_parts();
        if let Ok(reassembled) = assemble_ava_block(parts, &spec) {
            assert_eq!(
                reassembled.encoded_bytes(),
                data,
                "decode → assemble round-trip must be byte-identical"
            );
        }
    }
});
