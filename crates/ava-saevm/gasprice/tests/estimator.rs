// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Integration tests for the SAE gas-price/fee-history estimator. Ported from
//! Go `estimator_test.go`.

use std::sync::{Arc, Mutex};

use ava_saevm_gasprice::{
    Backend, BackendError, Block, BlockNumberRef, Clock, Config, ConfigError, Estimator,
    EstimatorError, MAX_PERCENTILES, Tx,
};
use ava_saevm_types::U256;

const N_AVAX: u64 = 1_000_000_000; // GWei
const A_AVAX: u64 = 1; // Wei
const GAS_LIMIT: u64 = 1_000_000;
const GAS_LIMIT_F64: f64 = 1_000_000.0;

/// A fake in-memory chain `Backend` over a synthetic chain.
///
/// Block 0 is genesis (added by the test setup). Heights index into `blocks`.
#[derive(Default)]
struct FakeChain {
    inner: Mutex<FakeChainInner>,
}

#[derive(Default)]
struct FakeChainInner {
    blocks: Vec<Block>,
    /// Optional next-block upper-bound base fee (worst-case bounds).
    next_base_fee: Option<U256>,
}

impl FakeChain {
    fn new() -> Self {
        Self::default()
    }

    /// Pushes a block at the next height with the given timestamp, base fee, and
    /// (gas, tip) txs. Txs are stored sorted ascending by tip (as the real
    /// backend would project them).
    fn push_block(&self, timestamp: u64, base_fee: U256, txs: Vec<Tx>) {
        let mut inner = self.inner.lock().unwrap();
        let gas_used: u64 = txs.iter().map(|t| t.gas).sum();
        let mut txs = txs;
        txs.sort_by_key(|t| t.tip);
        inner.blocks.push(Block {
            timestamp,
            gas_used,
            gas_limit: GAS_LIMIT,
            base_fee,
            txs,
        });
    }

    fn set_next_base_fee(&self, bf: Option<U256>) {
        self.inner.lock().unwrap().next_base_fee = bf;
    }
}

fn len_u64<T>(v: &[T]) -> u64 {
    u64::try_from(v.len()).unwrap_or(u64::MAX)
}

impl Backend for &FakeChain {
    fn resolve_block_number(&self, bn: BlockNumberRef) -> Result<u64, BackendError> {
        let inner = self.inner.lock().unwrap();
        let count = len_u64(&inner.blocks);
        let last = count.saturating_sub(1);
        match bn {
            BlockNumberRef::Earliest => Ok(0),
            BlockNumberRef::Latest | BlockNumberRef::Pending => Ok(last),
            BlockNumberRef::Number(n) => {
                if n < count {
                    Ok(n)
                } else {
                    Err(BackendError::BlockNotFound)
                }
            }
        }
    }

    fn block_by_number(&self, n: u64) -> Option<Block> {
        let inner = self.inner.lock().unwrap();
        usize::try_from(n)
            .ok()
            .and_then(|i| inner.blocks.get(i).cloned())
    }

    fn last_accepted_number(&self) -> u64 {
        let inner = self.inner.lock().unwrap();
        len_u64(&inner.blocks).saturating_sub(1)
    }

    fn next_block_upper_bound_base_fee(&self) -> Option<U256> {
        self.inner.lock().unwrap().next_base_fee
    }
}

fn frozen_clock(now: u64) -> Clock {
    Arc::new(move || now)
}

fn tx(gas: u64, tip: u64) -> Tx {
    Tx {
        gas,
        tip: U256::from(tip),
    }
}

// -----------------------------------------------------------------------------
// config_validate
// -----------------------------------------------------------------------------

#[test]
fn config_validate() {
    // default is valid
    assert!(Config::default_config().validate().is_ok());

    // percentile == 0
    let mut c = Config::default_config();
    c.suggested_tip_percentile = 0;
    assert_eq!(c.validate(), Err(ConfigError::BadTipPercentile));

    // percentile > 100
    let mut c = Config::default_config();
    c.suggested_tip_percentile = 101;
    assert_eq!(c.validate(), Err(ConfigError::BadTipPercentile));

    // min > max
    let mut c = Config::default_config();
    c.min_suggested_tip = U256::from(200u64);
    c.max_suggested_tip = U256::from(100u64);
    assert_eq!(c.validate(), Err(ConfigError::MinTipExceedsMax));
}

// -----------------------------------------------------------------------------
// suggest_gas_tip_cap (a.k.a. gas_price_uses_executed_base_fee)
// -----------------------------------------------------------------------------

fn suggest_cfg(now: u64) -> Config {
    let mut c = Config::default_config();
    c.min_suggested_tip = U256::from(A_AVAX);
    c.max_suggested_tip = U256::from(1_000_000_000_000_000_000u128); // AVAX
    c.now = frozen_clock(now);
    c
}

#[test]
#[allow(clippy::too_many_lines)] // table-driven test: the table dominates.
fn gas_price_uses_executed_base_fee() {
    struct Spec {
        time: u64,
        tips: Vec<u64>,
    }
    struct Case {
        name: &'static str,
        blocks: Vec<Spec>,
        want: U256,
    }

    let now: u64 = 100;
    let max_dur = suggest_cfg(now).suggested_tip_max_duration.as_secs();

    let cases = vec![
        Case {
            name: "genesis",
            blocks: vec![],
            want: U256::from(A_AVAX), // min
        },
        Case {
            name: "single_tx",
            blocks: vec![Spec {
                time: now,
                tips: vec![N_AVAX],
            }],
            want: U256::from(N_AVAX),
        },
        Case {
            name: "multiple_blocks",
            blocks: vec![
                Spec {
                    time: now - 10,
                    tips: vec![N_AVAX],
                },
                Spec {
                    time: now,
                    tips: vec![3 * N_AVAX, 2 * N_AVAX],
                },
            ],
            want: U256::from(N_AVAX),
        },
        Case {
            name: "increase_tip",
            blocks: vec![
                Spec {
                    time: now - 20,
                    tips: vec![N_AVAX],
                },
                Spec {
                    time: now - 10,
                    tips: vec![3 * N_AVAX, 2 * N_AVAX],
                },
                Spec {
                    time: now,
                    tips: vec![4 * N_AVAX],
                },
            ],
            want: U256::from(2 * N_AVAX),
        },
        Case {
            name: "min_tip",
            blocks: vec![Spec {
                time: now,
                tips: vec![1],
            }],
            want: U256::from(A_AVAX),
        },
        Case {
            name: "exceed_max_tip",
            blocks: vec![
                Spec {
                    time: now - 10,
                    tips: vec![u64::MAX],
                },
                Spec {
                    time: now,
                    tips: vec![u64::MAX],
                },
            ],
            want: U256::from(1_000_000_000_000_000_000u128), // max (AVAX)
        },
        Case {
            name: "exceed_max_duration",
            blocks: vec![
                Spec {
                    time: now - (max_dur + 1),
                    tips: vec![u64::MAX, u64::MAX, u64::MAX],
                },
                Spec {
                    time: now,
                    tips: vec![N_AVAX],
                },
            ],
            want: U256::from(N_AVAX),
        },
        Case {
            name: "no_transactions_fallback_to_last_price",
            blocks: vec![
                Spec {
                    time: now,
                    tips: vec![N_AVAX],
                },
                Spec {
                    time: now,
                    tips: vec![],
                },
            ],
            want: U256::from(N_AVAX),
        },
    ];

    for case in cases {
        let cfg = suggest_cfg(now);
        let chain = FakeChain::new();
        // genesis block at height 0 (matches Go: genesis exists before adds).
        chain.push_block(now, U256::ZERO, vec![]);
        for spec in &case.blocks {
            let txs = spec.tips.iter().map(|&t| tx(1, t)).collect();
            chain.push_block(spec.time, U256::from(1u64), txs);
        }

        let est = Estimator::new(&chain, cfg).unwrap();
        let got = est.suggest_gas_tip_cap();
        assert_eq!(got, case.want, "case {}", case.name);
    }
}

// -----------------------------------------------------------------------------
// fee_history_percentiles
// -----------------------------------------------------------------------------

fn fee_cfg() -> Config {
    let mut c = Config::default_config();
    c.history_max_blocks_from_head = 1;
    c.history_max_blocks = 2;
    c
}

/// The worst-case-bounds next-block base fee used by query_latest-style cases.
const BOUNDS_NEXT_BASE_FEE: u64 = 7;

#[test]
fn fee_history_percentiles_validation() {
    let chain = FakeChain::new();
    chain.push_block(0, U256::from(1u64), vec![]); // genesis
    let est = Estimator::new(&chain, fee_cfg()).unwrap();

    // too many percentiles
    let too_many = vec![0.0f64; MAX_PERCENTILES + 1];
    assert_eq!(
        est.fee_history(1, BlockNumberRef::Latest, &too_many),
        Err(EstimatorError::BadPercentile)
    );

    // out of range
    assert_eq!(
        est.fee_history(1, BlockNumberRef::Latest, &[-1.0]),
        Err(EstimatorError::BadPercentile)
    );

    // duplicate / not strictly ascending
    assert_eq!(
        est.fee_history(1, BlockNumberRef::Latest, &[1.0, 1.0]),
        Err(EstimatorError::BadPercentile)
    );

    // future block (numbered beyond head)
    assert!(matches!(
        est.fee_history(1, BlockNumberRef::Number(1), &[]),
        Err(EstimatorError::Backend(BackendError::BlockNotFound))
    ));
}

#[test]
fn fee_history_no_blocks() {
    let chain = FakeChain::new();
    chain.push_block(0, U256::from(1u64), vec![]); // genesis at height 0
    let est = Estimator::new(&chain, fee_cfg()).unwrap();

    // earliest block with numBlocks=0 -> blocks capped to 0 -> empty result.
    let (first, rewards, base_fees, ratio) =
        est.fee_history(0, BlockNumberRef::Earliest, &[]).unwrap();
    assert_eq!(first, 0);
    assert!(rewards.is_empty());
    assert!(base_fees.is_empty());
    assert!(ratio.is_empty());
}

#[test]
fn fee_history_nil_bounds_genesis_only() {
    // genesis only, no next-block bounds -> fall back to last header base fee.
    let chain = FakeChain::new();
    let initial_base_fee = U256::from(1_000_000_000u64);
    chain.push_block(0, initial_base_fee, vec![]); // genesis at height 0
    chain.set_next_base_fee(None);
    let est = Estimator::new(&chain, fee_cfg()).unwrap();

    let (first, rewards, base_fees, ratio) =
        est.fee_history(1, BlockNumberRef::Latest, &[]).unwrap();
    assert_eq!(first, 0);
    assert!(rewards.is_empty());
    // base fee of block 0, plus fallback (= block 0's base fee again)
    assert_eq!(base_fees, vec![initial_base_fee, initial_base_fee]);
    assert_eq!(ratio, vec![0.0]);
}

#[test]
fn fee_history_query_latest_with_bounds() {
    // height 0 = genesis (base_fee 1000), height 1 = block (base_fee 1).
    let chain = FakeChain::new();
    chain.push_block(0, U256::from(1_000_000_000u64), vec![]); // genesis
    chain.push_block(0, U256::from(1u64), vec![tx(21_000, N_AVAX)]);
    chain.set_next_base_fee(Some(U256::from(BOUNDS_NEXT_BASE_FEE)));
    let est = Estimator::new(&chain, fee_cfg()).unwrap();

    let (first, rewards, base_fees, ratio) =
        est.fee_history(1, BlockNumberRef::Latest, &[]).unwrap();
    assert_eq!(first, 1);
    assert!(rewards.is_empty());
    assert_eq!(
        base_fees,
        vec![U256::from(1u64), U256::from(BOUNDS_NEXT_BASE_FEE)]
    );
    // 21_000 / GAS_LIMIT
    assert_eq!(ratio, vec![21_000.0 / GAS_LIMIT_F64]);
}

#[test]
fn fee_history_query_too_old_block() {
    // history_max_blocks_from_head = 1; two real blocks => earliest (0) is too old.
    let chain = FakeChain::new();
    chain.push_block(0, U256::from(1u64), vec![]); // genesis (height 0)
    chain.push_block(0, U256::from(1u64), vec![tx(21_000, N_AVAX)]); // height 1
    chain.push_block(0, U256::from(2u64), vec![tx(100_000, N_AVAX)]); // height 2
    let est = Estimator::new(&chain, fee_cfg()).unwrap();

    // earliest (height 0); last_accepted=2, max_from_head=1 -> min_last=1 > 0.
    assert_eq!(
        est.fee_history(1, BlockNumberRef::Earliest, &[]),
        Err(EstimatorError::HistoryDepthExhausted)
    );
}

#[test]
fn fee_history_percentiles() {
    // Mirrors Go "query_max_blocks_with_percentiles".
    // height 0 genesis, height 1 (base_fee 1, one 21k tx @ nAVAX),
    // height 2 (base_fee 2, five 100k txs @ 1..5 nAVAX). max_blocks=2 caps to last 2.
    let chain = FakeChain::new();
    chain.push_block(0, U256::from(1_000_000_000u64), vec![]); // genesis height 0
    chain.push_block(0, U256::from(1u64), vec![tx(21_000, N_AVAX)]); // height 1
    chain.push_block(
        0,
        U256::from(2u64),
        vec![
            tx(100_000, N_AVAX),
            tx(100_000, 2 * N_AVAX),
            tx(100_000, 3 * N_AVAX),
            tx(100_000, 4 * N_AVAX),
            tx(100_000, 5 * N_AVAX),
        ],
    ); // height 2
    chain.set_next_base_fee(Some(U256::from(BOUNDS_NEXT_BASE_FEE)));
    let est = Estimator::new(&chain, fee_cfg()).unwrap();

    let (first, rewards, base_fees, ratio) = est
        .fee_history(u64::MAX, BlockNumberRef::Latest, &[25.0, 50.0, 75.0])
        .unwrap();
    assert_eq!(first, 1);
    assert_eq!(
        rewards,
        vec![
            vec![U256::from(N_AVAX), U256::from(N_AVAX), U256::from(N_AVAX)],
            vec![
                U256::from(2 * N_AVAX),
                U256::from(3 * N_AVAX),
                U256::from(4 * N_AVAX),
            ],
        ]
    );
    assert_eq!(
        base_fees,
        vec![
            U256::from(1u64),
            U256::from(2u64),
            U256::from(BOUNDS_NEXT_BASE_FEE)
        ]
    );
    assert_eq!(
        ratio,
        vec![21_000.0 / GAS_LIMIT_F64, 500_000.0 / GAS_LIMIT_F64]
    );
}
