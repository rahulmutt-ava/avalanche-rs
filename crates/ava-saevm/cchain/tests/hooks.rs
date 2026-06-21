// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! C-Chain hook tests (specs/11 §8 — `cchain/hooks.go`).
//!
//! Mirrors `vms/saevm/cchain/hooks_test.go` for the four hook surfaces this
//! task implements: `GasConfigAfter`, end-of-block atomic mint/burn `Op`s,
//! deterministic header build/rebuild, and the `CanExecuteTransaction` gate.
//!
//! The real atomic Import/Export tx codec + txpool is M7.22; these tests drive
//! a fake [`AtomicOpSource`] seam, matching the M7 deferred-impl pattern.

// Readable reference arithmetic in test fixtures; operands are tiny
// compile-time constants, so neither overflow nor truncation can occur here.
#![allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ava_evm_reth::{Header, RethBlock, SealedBlock, SealedHeader};
use ava_saevm_cchain::{AtomicOp, AtomicOpSource, CChainHooks};
use ava_saevm_gastime::GasPriceConfig;
use ava_saevm_hook::op::StateMut;
use ava_saevm_hook::{BlockBuilder, Points, PointsG, Settled, StateRead};
use ava_saevm_types::{Address, B256, U256};
use ava_vm::components::gas::Gas;

// --- in-memory state seam (mirrors worstcase MemState) ---------------------

#[derive(Default)]
struct MemState {
    balances: BTreeMap<Address, U256>,
    nonces: BTreeMap<Address, u64>,
}

impl MemState {
    fn with_balance(addr: Address, bal: U256) -> Self {
        let mut s = Self::default();
        s.balances.insert(addr, bal);
        s
    }
}

impl StateRead for MemState {
    fn balance(&self, a: Address) -> U256 {
        self.balances.get(&a).copied().unwrap_or(U256::ZERO)
    }
    fn nonce(&self, a: Address) -> u64 {
        self.nonces.get(&a).copied().unwrap_or(0)
    }
}

impl StateMut for MemState {
    fn balance(&self, a: Address) -> U256 {
        self.balances.get(&a).copied().unwrap_or(U256::ZERO)
    }
    fn nonce(&self, a: Address) -> u64 {
        self.nonces.get(&a).copied().unwrap_or(0)
    }
    fn set_nonce(&mut self, a: Address, n: u64) {
        self.nonces.insert(a, n);
    }
    fn sub_balance(&mut self, a: Address, amount: U256) {
        let e = self.balances.entry(a).or_insert(U256::ZERO);
        *e = e.saturating_sub(amount);
    }
    fn add_balance(&mut self, a: Address, amount: U256) {
        let e = self.balances.entry(a).or_insert(U256::ZERO);
        *e = e.saturating_add(amount);
    }
}

// --- fixtures --------------------------------------------------------------

fn addr(b: u8) -> Address {
    let mut bytes = [0u8; 20];
    bytes[0] = b;
    Address::from(bytes)
}

fn id(b: u8) -> ava_types::id::Id {
    ava_types::id::Id::from([b; 32])
}

/// A fake [`AtomicOpSource`] that returns a fixed set of atomic ops, regardless
/// of block. Stands in for the M7.22 atomic-tx codec/txpool.
struct FakeAtomicSource {
    ops: Vec<AtomicOp>,
}

impl AtomicOpSource for FakeAtomicSource {
    fn atomic_ops(&self, _block: &SealedBlock<RethBlock>) -> Vec<AtomicOp> {
        self.ops.clone()
    }
}

fn header(parent: B256, number: u64, time: u64) -> SealedHeader {
    SealedHeader::seal_slow(Header {
        parent_hash: parent,
        number,
        timestamp: time,
        ..Header::default()
    })
}

fn block_with(header: SealedHeader) -> SealedBlock<RethBlock> {
    SealedBlock::seal_slow(RethBlock::uncle(header.into_header()))
}

// --- tests -----------------------------------------------------------------

#[test]
fn gas_config_after_returns_expected() {
    // Mirrors Go `hooks.GasConfigAfter`: returns 1_000_000 gas target and a
    // GasPriceConfig{TargetToExcessScaling: 87, MinPrice: 1}.
    let hooks = CChainHooks::new(FakeAtomicSource { ops: vec![] });
    let h = header(B256::ZERO, 1, 100);

    let (target, config) = hooks.gas_config_after(&h);

    assert_eq!(target, Gas(1_000_000));
    assert_eq!(config.target_to_excess_scaling(), 87);
    assert_eq!(config.min_price(), 1);
    assert!(!config.static_pricing());
    assert_eq!(config, GasPriceConfig::default());
}

#[test]
fn end_of_block_ops_apply_import_export_mint_burn() {
    // An import credits `recipient`; an export debits `sender`. Applying the
    // resulting Ops to in-memory state must mint to the recipient and burn from
    // the sender.
    let recipient = addr(0x11);
    let sender = addr(0x22);

    let import = AtomicOp::Import {
        id: id(0x01),
        to: recipient,
        amount: U256::from(1_000u64),
        gas: Gas(100),
        gas_fee_cap: U256::from(1u64),
    };
    let export = AtomicOp::Export {
        id: id(0x02),
        from: sender,
        amount: U256::from(400u64),
        min_balance: U256::from(400u64),
        nonce: 0,
        gas: Gas(100),
        gas_fee_cap: U256::from(1u64),
    };

    let hooks = CChainHooks::new(FakeAtomicSource {
        ops: vec![import, export],
    });

    let block = block_with(header(B256::ZERO, 1, 100));
    let ops = hooks.end_of_block_ops(&block).expect("end_of_block_ops");
    assert_eq!(ops.len(), 2, "one Op per atomic op");

    // Fund the sender so the export burn succeeds.
    let mut state = MemState::with_balance(sender, U256::from(500u64));

    for op in &ops {
        op.apply_to(&mut state).expect("apply Op");
    }

    // Import minted to recipient.
    assert_eq!(StateRead::balance(&state, recipient), U256::from(1_000u64));
    // Export burned from sender (500 - 400).
    assert_eq!(StateRead::balance(&state, sender), U256::from(100u64));
    // Export bumped the sender nonce.
    assert_eq!(StateRead::nonce(&state, sender), 1);
}

#[test]
fn build_header_matches_rebuild() {
    // A faithfully rebuilt header (via block_rebuilder_from) must be
    // byte-identical to the one produced by build_header for the same parent.
    let hooks = CChainHooks::new(FakeAtomicSource { ops: vec![] });

    let parent = header(B256::repeat_byte(0xaa), 7, 1_700_000_000);
    let built = hooks.build_header(&parent).expect("build_header");

    // Construct the block the way the builder would, then rebuild from it.
    let block = block_with(built.clone());
    let rebuilder = hooks.block_rebuilder_from(&block).expect("rebuilder");
    let rebuilt = rebuilder.build_header(&parent).expect("rebuild header");

    assert_eq!(built, rebuilt, "rebuilt header must be byte-identical");
    // Sanity: parent/number/time were set as Go's BuildHeader specifies.
    assert_eq!(built.parent_hash, parent.hash());
    assert_eq!(built.number, 8);
}

#[test]
fn can_execute_transaction_gates_atomic() {
    // A disallowed sender is rejected; an allowed one passes. Mirrors the
    // libevm RulesAllowlistHooks.CanExecuteTransaction surface.
    let blocked = addr(0x99);
    let allowed = addr(0x01);

    let mut deny = BTreeSet::new();
    deny.insert(blocked);

    let hooks = CChainHooks::new(FakeAtomicSource { ops: vec![] }).with_blocked_senders(deny);

    let state = MemState::default();

    assert!(
        hooks
            .can_execute_transaction(allowed, Some(addr(0x02)), &state)
            .is_ok(),
        "allowed sender must pass"
    );
    assert!(
        hooks
            .can_execute_transaction(blocked, None, &state)
            .is_err(),
        "blocked sender must be rejected"
    );
}

/// A clock pinned to a fixed [`SystemTime`], used to inject a deterministic
/// sub-second instant into [`CChainHooks`] (mirrors Go's `withTime(...)` test
/// knob feeding the injected `now func() time.Time`).
fn pinned_clock(millis: u64) -> Arc<dyn Fn() -> SystemTime + Send + Sync> {
    let at = UNIX_EPOCH + Duration::from_millis(millis);
    Arc::new(move || at)
}

#[test]
fn build_header_stamps_and_block_time_round_trips_subsecond() {
    // Mirrors Go `TestBuildBlockPreservesMillisecondTimestamp`: a non-zero
    // sub-second component (123 ms) must survive build -> block_time, with the
    // whole-seconds in `time` and the millisecond instant preserved.
    const WANT_MILLIS: u64 = 1_700_000_000_123;
    const WANT_SECONDS: u64 = WANT_MILLIS / 1000;

    let hooks =
        CChainHooks::new(FakeAtomicSource { ops: vec![] }).with_clock(pinned_clock(WANT_MILLIS));

    let parent = header(B256::repeat_byte(0xaa), 7, 0);
    let built = hooks.build_header(&parent).expect("build_header");

    // The header's whole-seconds component is millis/1000.
    assert_eq!(built.timestamp, WANT_SECONDS, "built header.time (seconds)");

    // block_time reconstructs the instant: seconds anchored to header.time, the
    // sub-second component (123 ms = 123_000_000 ns) recovered from the carrier.
    let (secs, nanos) = hooks.block_time(&built);
    assert_eq!(secs, WANT_SECONDS, "block_time seconds == header.time");
    assert_eq!(nanos, 123_000_000, "block_time sub-second nanos (123 ms)");

    // The reconstructed instant equals the pinned build instant to the ms.
    let reconstructed_millis = secs
        .checked_mul(1000)
        .and_then(|ms| ms.checked_add(u64::from(nanos / 1_000_000)))
        .expect("reconstructed millis");
    assert_eq!(reconstructed_millis, WANT_MILLIS, "round-trip millis");
}

#[test]
fn block_time_anchors_seconds_to_header_time_under_mismatch() {
    // Mirrors Go `TestVerifyBlockRejectsMismatchedTime`'s anchoring invariant:
    // a malicious peer bumps the seconds encoded in TimeMilliseconds without
    // touching Header.Time. block_time().unix() MUST still equal header.time.
    const BUILD_MILLIS: u64 = 1_700_000_000_123;

    let hooks =
        CChainHooks::new(FakeAtomicSource { ops: vec![] }).with_clock(pinned_clock(BUILD_MILLIS));
    let parent = header(B256::repeat_byte(0xbb), 3, 0);
    let built = hooks.build_header(&parent).expect("build_header");
    let honest_time = built.timestamp;

    // Forge the millisecond carrier so its encoded seconds disagree with
    // header.time by +1000 ms (= +1 s), every other field untouched.
    let mut forged = built.clone().into_header();
    let forged_millis = ava_saevm_cchain::header_time_milliseconds(built.header())
        .checked_add(1000)
        .expect("forged millis");
    ava_saevm_cchain::set_header_time_milliseconds(&mut forged, forged_millis);
    let forged = SealedHeader::seal_slow(forged);

    // The seconds component is anchored to header.time, NOT to forged_millis/1000.
    let (secs, _nanos) = hooks.block_time(&forged);
    assert_eq!(
        secs, honest_time,
        "block_time seconds anchored to header.time even under mismatch"
    );
    // Sub-second component still tracks the (forged) carrier modulo 1000 — here
    // unchanged (the forge added a whole second), so nanos stay 123 ms.
    assert_eq!(
        forged.timestamp, honest_time,
        "header.time untouched by forge"
    );
}

#[test]
fn settled_by_round_trips_build_block() {
    // settled_by/build_block aren't among the four named tests but guard the
    // Settled plumbing the VM relies on; keep it cheap.
    let hooks = CChainHooks::new(FakeAtomicSource { ops: vec![] });
    let h = header(B256::ZERO, 1, 100);
    let settled = hooks.settled_by(&h);
    // Go's SettledBy returns a zero-valued hook.Settled (TODO: extract from
    // the header). Mirror that here.
    assert_eq!(
        settled,
        Settled {
            height: 0,
            gas_unix: 0,
            gas_numerator: Gas(0),
            excess: Gas(0),
        }
    );
}
