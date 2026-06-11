// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Shared hand-rolled ABI word helpers + `InterpreterResult` constructors for
//! the ConfigKey stateful precompiles (M6.31, spec 10 §8). Mirrors the geth
//! `accounts/abi` decode semantics the subnet-evm precompiles rely on:
//!
//! - **address**: the rightmost 20 bytes of the word; the high 12 bytes are NOT
//!   validated (geth `common.BytesToAddress`).
//! - **uint256**: the full word.
//! - **uint64**: the low 8 bytes; the high 24 bytes must be zero (geth errors
//!   with "abi: improperly encoded uint64").
//! - **bool**: the low byte must be 0/1 and the high 31 bytes zero (geth
//!   "abi: improperly encoded boolean value").
//! - **strict length** (pre-Durango `useStrictMode`): the input must be exactly
//!   `n_words * 32` bytes; post-Durango extra trailing padding is tolerated but
//!   the input must still carry at least the required words.

use ava_evm_reth::{Address, Bytes, Gas, InstructionResult, InterpreterResult, U256};

/// One 32-byte ABI word.
pub(crate) const WORD: usize = 32;

/// Checks the argument length for `n_words` static words: exact under
/// `strict` (pre-Durango), at-least otherwise.
pub(crate) fn check_args_len(args: &[u8], n_words: usize, strict: bool) -> bool {
    let need = n_words.saturating_mul(WORD);
    if strict {
        args.len() == need
    } else {
        args.len() >= need
    }
}

/// The `i`-th 32-byte word of `args`, if present.
pub(crate) fn word_at(args: &[u8], i: usize) -> Option<&[u8]> {
    let start = i.checked_mul(WORD)?;
    let end = start.checked_add(WORD)?;
    args.get(start..end)
}

/// Reads an `address` argument (rightmost 20 bytes; high bytes ignored — geth
/// `common.BytesToAddress` parity).
pub(crate) fn read_addr(args: &[u8], i: usize) -> Option<Address> {
    let w = word_at(args, i)?;
    Some(Address::from_slice(&w[12..32]))
}

/// Reads a `uint256` argument (the full word).
pub(crate) fn read_u256(args: &[u8], i: usize) -> Option<U256> {
    let w = word_at(args, i)?;
    let mut b = [0u8; 32];
    b.copy_from_slice(w);
    Some(U256::from_be_bytes(b))
}

/// Reads a `uint64` argument (high 24 bytes must be zero — geth parity).
pub(crate) fn read_u64(args: &[u8], i: usize) -> Option<u64> {
    let w = word_at(args, i)?;
    if w[0..24].iter().any(|&b| b != 0) {
        return None;
    }
    let mut b = [0u8; 8];
    b.copy_from_slice(&w[24..32]);
    Some(u64::from_be_bytes(b))
}

/// Reads a `bool` argument (low byte 0/1, high 31 bytes zero — geth parity).
pub(crate) fn read_bool(args: &[u8], i: usize) -> Option<bool> {
    let w = word_at(args, i)?;
    if w[0..31].iter().any(|&b| b != 0) {
        return None;
    }
    match w[31] {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

/// An `address` packed into a left-zero-padded word.
pub(crate) fn word_addr(a: Address) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..32].copy_from_slice(a.as_slice());
    w
}

/// A `uint64` packed into a word.
pub(crate) fn word_u64(v: u64) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[24..32].copy_from_slice(&v.to_be_bytes());
    w
}

/// A `bool` packed into a word.
pub(crate) fn word_bool(v: bool) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[31] = u8::from(v);
    w
}

/// A `uint256` packed into a word.
pub(crate) fn word_u256(v: U256) -> [u8; 32] {
    v.to_be_bytes::<32>()
}

/// A successful precompile return: `Return`, `output`, with `gas` carrying the
/// already-recorded cost.
pub(crate) fn success(output: Vec<u8>, gas: Gas) -> InterpreterResult {
    InterpreterResult {
        result: InstructionResult::Return,
        output: Bytes::from(output),
        gas,
    }
}

/// A user-triggerable precompile call failure (all supplied gas consumed —
/// geth consumes the call frame's gas on a non-revert precompile error).
pub(crate) fn failure(gas_limit: u64) -> InterpreterResult {
    let mut g = Gas::new(gas_limit);
    g.spend_all();
    InterpreterResult {
        result: InstructionResult::PrecompileError,
        output: Bytes::new(),
        gas: g,
    }
}

/// An out-of-gas precompile result (all supplied gas consumed).
pub(crate) fn out_of_gas(gas_limit: u64) -> InterpreterResult {
    let mut g = Gas::new(gas_limit);
    g.spend_all();
    InterpreterResult {
        result: InstructionResult::PrecompileOOG,
        output: Bytes::new(),
        gas: g,
    }
}
