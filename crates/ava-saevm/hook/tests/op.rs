// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Ports `vms/saevm/hook/hook_test.go::TestOp_ApplyTo` plus a self-consistent
//! golden for `Settled::settled_gas_time` (specs/11 §9.1). Exact Go-canoto
//! parity for the gas-clock golden is deferred to M7.29.

use std::collections::BTreeMap;

use ava_saevm_gastime::{GasPriceConfig, GasTime};
use ava_saevm_hook::op::{AccountDebit, Op, OpError, StateMut};
use ava_saevm_hook::settled::Settled;
use ava_saevm_types::{Address, U256};
use ava_vm::components::gas::Gas;

/// A `BTreeMap`-backed fake `StateMut` for testing `Op::apply_to`.
#[derive(Default)]
struct FakeState {
    // address -> (nonce, balance)
    accounts: BTreeMap<Address, (u64, U256)>,
}

impl StateMut for FakeState {
    fn balance(&self, a: Address) -> U256 {
        self.accounts.get(&a).map_or(U256::ZERO, |v| v.1)
    }

    fn nonce(&self, a: Address) -> u64 {
        self.accounts.get(&a).map_or(0, |v| v.0)
    }

    fn set_nonce(&mut self, a: Address, n: u64) {
        self.accounts.entry(a).or_insert((0, U256::ZERO)).0 = n;
    }

    fn sub_balance(&mut self, a: Address, amount: U256) {
        let e = self.accounts.entry(a).or_insert((0, U256::ZERO));
        e.1 = e.1.saturating_sub(amount);
    }

    fn add_balance(&mut self, a: Address, amount: U256) {
        let e = self.accounts.entry(a).or_insert((0, U256::ZERO));
        e.1 = e.1.saturating_add(amount);
    }
}

fn addr(b: u8) -> Address {
    let mut bytes = [0u8; 20];
    bytes[0] = b;
    Address::from(bytes)
}

fn burn(amount: u64, min_balance: u64) -> AccountDebit {
    AccountDebit {
        nonce: 0,
        amount: U256::from(amount),
        min_balance: U256::from(min_balance),
    }
}

struct WantAccount {
    address: Address,
    nonce: u64,
    balance: U256,
}

struct Step {
    name: &'static str,
    op: Op,
    want_accounts: Vec<WantAccount>,
    want_err: Option<OpError>,
}

#[test]
#[allow(clippy::too_many_lines)]
fn op_apply_to() {
    let eoa = addr(0x00);
    let eoa_max_nonce = addr(0x01);

    let mut db = FakeState::default();
    db.set_nonce(eoa_max_nonce, u64::MAX);

    let steps = vec![
        Step {
            name: "mint_to_eoa",
            op: Op {
                mint: BTreeMap::from([(eoa, U256::from(1_000_000u64))]),
                ..Op::empty()
            },
            want_accounts: vec![
                WantAccount {
                    address: eoa,
                    nonce: 0,
                    balance: U256::from(1_000_000u64),
                },
                WantAccount {
                    address: eoa_max_nonce,
                    nonce: u64::MAX,
                    balance: U256::ZERO,
                },
            ],
            want_err: None,
        },
        Step {
            name: "transfer_from_eoa_to_eoa_max_nonce",
            op: Op {
                burn: BTreeMap::from([(eoa, burn(100_000, 100_000))]),
                mint: BTreeMap::from([(eoa_max_nonce, U256::from(100_000u64))]),
                ..Op::empty()
            },
            want_accounts: vec![
                WantAccount {
                    address: eoa,
                    nonce: 1,
                    balance: U256::from(900_000u64),
                },
                WantAccount {
                    address: eoa_max_nonce,
                    nonce: u64::MAX,
                    balance: U256::from(100_000u64),
                },
            ],
            want_err: None,
        },
        Step {
            name: "burn_all_funds",
            op: Op {
                burn: BTreeMap::from([
                    (eoa, burn(900_000, 900_000)),
                    (eoa_max_nonce, burn(100_000, 100_000)),
                ]),
                ..Op::empty()
            },
            want_accounts: vec![
                WantAccount {
                    address: eoa,
                    nonce: 2,
                    balance: U256::ZERO,
                },
                WantAccount {
                    address: eoa_max_nonce,
                    nonce: u64::MAX, // unchanged
                    balance: U256::ZERO,
                },
            ],
            want_err: None,
        },
        Step {
            name: "insufficient_funds",
            op: Op {
                burn: BTreeMap::from([(eoa, burn(1, 1))]),
                ..Op::empty()
            },
            want_accounts: vec![],
            want_err: Some(OpError::InsufficientFunds),
        },
        Step {
            name: "fund_eoa_for_min_balance_tests",
            op: Op {
                mint: BTreeMap::from([(eoa, U256::from(500u64))]),
                ..Op::empty()
            },
            want_accounts: vec![WantAccount {
                address: eoa,
                nonce: 2,
                balance: U256::from(500u64),
            }],
            want_err: None,
        },
        Step {
            name: "balance_below_min_balance",
            op: Op {
                burn: BTreeMap::from([(eoa, burn(100, 1000))]),
                ..Op::empty()
            },
            want_accounts: vec![],
            want_err: Some(OpError::InsufficientFunds),
        },
        Step {
            name: "balance_covers_min_balance_debits_amount",
            op: Op {
                burn: BTreeMap::from([(eoa, burn(100, 500))]),
                ..Op::empty()
            },
            want_accounts: vec![WantAccount {
                address: eoa,
                nonce: 3,
                balance: U256::from(400u64),
            }],
            want_err: None,
        },
        Step {
            name: "min_balance_unset_does_not_allow_underflow",
            op: Op {
                burn: BTreeMap::from([(
                    eoa,
                    AccountDebit {
                        nonce: 0,
                        amount: U256::from(500u64),
                        min_balance: U256::ZERO,
                    },
                )]),
                ..Op::empty()
            },
            want_accounts: vec![WantAccount {
                address: eoa,
                nonce: 3,
                balance: U256::from(400u64),
            }],
            want_err: Some(OpError::MinBalanceBelowAmount),
        },
        Step {
            name: "min_balance_below_amount_does_not_allow_underflow",
            op: Op {
                burn: BTreeMap::from([(eoa, burn(500, 300))]),
                ..Op::empty()
            },
            want_accounts: vec![WantAccount {
                address: eoa,
                nonce: 3,
                balance: U256::from(400u64),
            }],
            want_err: Some(OpError::MinBalanceBelowAmount),
        },
    ];

    for step in steps {
        let got = step.op.apply_to(&mut db);
        match &step.want_err {
            None => assert!(got.is_ok(), "ApplyTo {} should succeed: {got:?}", step.name),
            Some(want) => {
                let err = got.expect_err(step.name);
                assert_eq!(
                    std::mem::discriminant(&err),
                    std::mem::discriminant(want),
                    "ApplyTo {} err",
                    step.name
                );
            }
        }
        for acct in &step.want_accounts {
            assert_eq!(
                db.nonce(acct.address),
                acct.nonce,
                "nonce of account after {}",
                step.name
            );
            assert_eq!(
                db.balance(acct.address),
                acct.balance,
                "balance of account after {}",
                step.name
            );
        }
    }
}

#[test]
fn settled_gas_time_roundtrip() {
    // A self-consistent golden: build a Settled, reconstruct the gas clock, and
    // assert the reconstructed target/excess match what `from_settled` derives.
    // Exact Go-canoto parity is deferred to M7.29.
    let target = Gas(1_000);
    let config = GasPriceConfig::default();

    let settled = Settled {
        height: 42,
        gas_unix: 1_700_000_000,
        gas_numerator: Gas(123),
        excess: Gas(50_000),
    };

    let gt = settled.settled_gas_time(target, config);

    // Cross-check against a direct from_settled call with identical inputs.
    let expected = GasTime::from_settled(
        settled.gas_unix,
        settled.gas_numerator.0,
        target.0,
        settled.excess.0,
        config,
    );

    assert_eq!(gt.target(), expected.target());
    assert_eq!(gt.excess(), expected.excess());
    assert_eq!(gt.rate(), expected.rate());

    // Frozen golden derived by hand from the inputs:
    //   rate = SafeRateOfTarget(1000) = clamp(1000) * 2 = 2000
    //   target (post-FromProxyTime) = rate / 2 = 1000
    //   excess (dynamic pricing) = max(50_000, enforce_min_excess floor)
    assert_eq!(gt.rate(), 2_000);
    assert_eq!(gt.target(), Gas(1_000));
    // excess >= the input since enforce_min_excess only raises it.
    assert!(gt.excess().0 >= 50_000);
}
