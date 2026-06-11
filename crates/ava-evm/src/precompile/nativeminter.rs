// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The ContractNativeMinter stateful precompile — port of subnet-evm
//! `precompile/contracts/nativeminter` (M6.31, spec 10 §8). Mints native coin
//! to an address, gated by the embedded allow list at its own address.

use std::sync::Arc;

use ava_evm_reth::{Address, B256, Gas, InterpreterResult, PrecompileError};

use crate::precompile::abi::{
    check_args_len, failure, out_of_gas, read_addr, read_u256, success, word_addr, word_u256,
};
use crate::precompile::allowlist::{dispatch_allowlist, get_allow_list_status, split_selector};
use crate::precompile::registry::{
    PrecompileCtx, PrecompileModule, PrecompileStateOps, StatefulPrecompile,
};

/// `nativeminter.ContractAddress` (`0x02..01`).
pub const NATIVE_MINTER_ADDRESS: Address = Address::new([
    0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x01,
]);

/// `MintGasCost` (`contract.go`).
pub const MINT_GAS: u64 = 30_000;
/// `NativeCoinMintedEventGasCost` — log + 3 topics + 32 data bytes (`event.go`).
pub const NATIVE_COIN_MINTED_EVENT_GAS: u64 = 375 + 375 * 3 + 8 * 32;

/// `mintNativeCoin(address,uint256)` selector.
pub const SEL_MINT_NATIVE_COIN: [u8; 4] = [0x4f, 0x5a, 0xaa, 0xba];

/// `keccak256("NativeCoinMinted(address,address,uint256)")` — topic0 (indexed:
/// sender, recipient; data: amount).
pub const NATIVE_COIN_MINTED_EVENT_TOPIC: [u8; 32] = [
    0x40, 0x0c, 0xd3, 0x92, 0xf3, 0xd5, 0x6f, 0xd1, 0x0b, 0xb1, 0xdb, 0xd5, 0x83, 0x9f, 0xdd, 0xa8,
    0x29, 0x82, 0x08, 0xdd, 0xaa, 0x97, 0xb3, 0x68, 0xfa, 0xa0, 0x53, 0xe1, 0x85, 0x09, 0x30, 0xee,
];

/// The NativeMinter precompile body.
#[derive(Clone, Copy, Debug, Default)]
pub struct NativeMinterPrecompile;

impl NativeMinterPrecompile {
    /// The registry module at [`NATIVE_MINTER_ADDRESS`], activated at
    /// `activation`.
    #[must_use]
    pub fn module(self, activation: u64) -> PrecompileModule {
        PrecompileModule {
            address: NATIVE_MINTER_ADDRESS,
            activation,
            precompile: Arc::new(self),
        }
    }
}

impl StatefulPrecompile for NativeMinterPrecompile {
    fn run(
        &self,
        input: &[u8],
        gas_limit: u64,
        ctx: &PrecompileCtx,
        state: &mut dyn PrecompileStateOps,
    ) -> Result<InterpreterResult, PrecompileError> {
        let Some((selector, args)) = split_selector(input) else {
            return Ok(failure(gas_limit));
        };
        if let Some(res) =
            dispatch_allowlist(NATIVE_MINTER_ADDRESS, selector, args, gas_limit, ctx, state)
        {
            return res;
        }
        if selector == SEL_MINT_NATIVE_COIN {
            return mint_native_coin(args, gas_limit, ctx, state);
        }
        Ok(failure(gas_limit))
    }
}

/// `mintNativeCoin` (`contract.go`): deduct gas → write-protection → unpack →
/// allow-list gate → Durango event → credit the balance.
fn mint_native_coin(
    args: &[u8],
    gas_limit: u64,
    ctx: &PrecompileCtx,
    state: &mut dyn PrecompileStateOps,
) -> Result<InterpreterResult, PrecompileError> {
    let mut g = Gas::new(gas_limit);
    if !g.record_regular_cost(MINT_GAS) {
        return Ok(out_of_gas(gas_limit));
    }
    if ctx.read_only {
        return Ok(failure(gas_limit));
    }
    if !check_args_len(args, 2, !ctx.block.is_durango) {
        return Ok(failure(gas_limit));
    }
    let (Some(to), Some(amount)) = (read_addr(args, 0), read_u256(args, 1)) else {
        return Ok(failure(gas_limit));
    };
    if !get_allow_list_status(state, NATIVE_MINTER_ADDRESS, ctx.caller)?.is_enabled() {
        // Go `ErrCannotMint`.
        return Ok(failure(gas_limit));
    }
    if ctx.block.is_durango {
        if !g.record_regular_cost(NATIVE_COIN_MINTED_EVENT_GAS) {
            return Ok(out_of_gas(gas_limit));
        }
        state.add_log(
            NATIVE_MINTER_ADDRESS,
            vec![
                B256::from(NATIVE_COIN_MINTED_EVENT_TOPIC),
                B256::new(word_addr(ctx.caller)),
                B256::new(word_addr(to)),
            ],
            word_u256(amount).to_vec(),
        );
    }
    // coreth creates the account if absent then `AddBalance`; the journal's
    // balance-increment does both.
    state.add_balance(to, amount)?;
    Ok(success(Vec::new(), g))
}
