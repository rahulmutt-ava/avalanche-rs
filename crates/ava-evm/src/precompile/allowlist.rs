// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The AllowList role machinery shared by every ConfigKey precompile, plus the
//! standalone allow-list precompiles (ContractDeployerAllowList /
//! TxAllowList) — port of subnet-evm `precompile/allowlist` (M6.31, spec 10
//! §8).
//!
//! Roles live in the embedding precompile's own storage, keyed by the
//! controlled address right-aligned into a 32-byte slot key
//! (`common.BytesToHash(address)`); the slot value is the role hash (0 = None,
//! 1 = Enabled, 2 = Admin, 3 = Manager — `role.go`). The five selectors
//! (`readAllowList`/`setAdmin`/`setEnabled`/`setManager`/`setNone`) are
//! embedded by NativeMinter/FeeManager/RewardManager/GasPriceManager at their
//! own addresses via [`dispatch_allowlist`]; `setManager` only exists
//! post-Durango (`CreateAllowListFunctions`' activation gate).

use std::sync::Arc;

use ava_evm_reth::{Address, B256, Gas, InterpreterResult, PrecompileError};

use crate::precompile::abi::{check_args_len, failure, out_of_gas, read_addr, success, word_addr};
use crate::precompile::registry::{
    PrecompileCtx, PrecompileModule, PrecompileStateOps, StatefulPrecompile,
};

/// `deployerallowlist.ContractAddress` (`0x02..00`).
pub const DEPLOYER_ALLOW_LIST_ADDRESS: Address = Address::new([
    0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00,
]);

/// `txallowlist.ContractAddress` (`0x02..02`).
pub const TX_ALLOW_LIST_ADDRESS: Address = Address::new([
    0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x02,
]);

/// `ModifyAllowListGasCost = contract.WriteGasCostPerSlot` (`allowlist.go`).
pub const MODIFY_ALLOW_LIST_GAS: u64 = 20_000;
/// `ReadAllowListGasCost = contract.ReadGasCostPerSlot`.
pub const READ_ALLOW_LIST_GAS: u64 = 5_000;
/// `AllowListEventGasCost` — log + 4 topics + 32 data bytes (`event.go`).
pub const ALLOW_LIST_EVENT_GAS: u64 = 375 + 375 * 4 + 8 * 32;

// ---- ABI selectors (Go `method.ID`, recomputed from the signatures) --------

/// `readAllowList(address)`.
pub const SEL_READ_ALLOW_LIST: [u8; 4] = [0xeb, 0x54, 0xda, 0xe1];
/// `setAdmin(address)`.
pub const SEL_SET_ADMIN: [u8; 4] = [0x70, 0x4b, 0x6c, 0x02];
/// `setEnabled(address)`.
pub const SEL_SET_ENABLED: [u8; 4] = [0x0a, 0xaf, 0x70, 0x43];
/// `setManager(address)` (Durango+).
pub const SEL_SET_MANAGER: [u8; 4] = [0xd0, 0xeb, 0xdb, 0xe7];
/// `setNone(address)`.
pub const SEL_SET_NONE: [u8; 4] = [0x8c, 0x6b, 0xfb, 0x3b];

/// `keccak256("RoleSet(uint256,address,address,uint256)")` — topic0 of the
/// Durango+ `RoleSet` event (indexed: role, account, sender; data: oldRole).
pub const ROLE_SET_EVENT_TOPIC: [u8; 32] = [
    0xcd, 0xb7, 0xea, 0x01, 0xf0, 0x0a, 0x41, 0x4d, 0x78, 0x75, 0x7b, 0xdb, 0x0f, 0x63, 0x91, 0x66,
    0x4b, 0xa3, 0xfe, 0xdf, 0x98, 0x7e, 0xed, 0x28, 0x09, 0x27, 0xc1, 0xe7, 0xd6, 0x95, 0xbe, 0x3e,
];

/// An allow-list role (`role.go`): the 32-byte storage value. Only the four
/// canonical values are constructible.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// `NoRole` (0) — deletes the key when set.
    None,
    /// `EnabledRole` (1) — may call the precompile.
    Enabled,
    /// `AdminRole` (2) — may call AND modify the allow list.
    Admin,
    /// `ManagerRole` (3, Durango+) — may add/remove Enabled addresses and call.
    Manager,
}

impl Role {
    /// The role's 32-byte storage word (`Role.Hash()`).
    #[must_use]
    pub fn word(self) -> B256 {
        let mut w = [0u8; 32];
        w[31] = match self {
            Role::None => 0,
            Role::Enabled => 1,
            Role::Admin => 2,
            Role::Manager => 3,
        };
        B256::new(w)
    }

    /// Decodes a storage word into a role; an unknown value reads as
    /// [`Role::None`] (Go's `Role(hash)` comparisons treat it as no role for
    /// `IsEnabled`/`CanModify`; `readAllowList` echoes the raw word, which
    /// [`get_allow_list_word`] preserves).
    #[must_use]
    pub fn from_word(w: B256) -> Role {
        if w == Role::Enabled.word() {
            Role::Enabled
        } else if w == Role::Admin.word() {
            Role::Admin
        } else if w == Role::Manager.word() {
            Role::Manager
        } else {
            Role::None
        }
    }

    /// `Role.IsEnabled()` — Admin, Enabled and Manager may call.
    #[must_use]
    pub fn is_enabled(self) -> bool {
        matches!(self, Role::Admin | Role::Enabled | Role::Manager)
    }

    /// `Role.CanModify(from, target)` — Admin modifies anything; a Manager only
    /// moves addresses between None and Enabled.
    #[must_use]
    pub fn can_modify(self, from: Role, target: Role) -> bool {
        match self {
            Role::Admin => true,
            Role::Manager => {
                matches!(from, Role::Enabled | Role::None)
                    && matches!(target, Role::Enabled | Role::None)
            }
            _ => false,
        }
    }
}

/// The allow-list slot key for `address` at any precompile:
/// `common.BytesToHash(address.Bytes())` — the address right-aligned.
#[must_use]
pub fn allow_list_key(address: Address) -> B256 {
    B256::new(word_addr(address))
}

/// `GetAllowListStatus` — the role of `address` for the precompile at
/// `precompile_addr`.
///
/// # Errors
/// Returns [`PrecompileError`] on an underlying state-read failure.
pub fn get_allow_list_status(
    state: &mut dyn PrecompileStateOps,
    precompile_addr: Address,
    address: Address,
) -> Result<Role, PrecompileError> {
    Ok(Role::from_word(get_allow_list_word(
        state,
        precompile_addr,
        address,
    )?))
}

/// The raw 32-byte role word (what `readAllowList` echoes back).
///
/// # Errors
/// Returns [`PrecompileError`] on an underlying state-read failure.
pub fn get_allow_list_word(
    state: &mut dyn PrecompileStateOps,
    precompile_addr: Address,
    address: Address,
) -> Result<B256, PrecompileError> {
    state.get_state(precompile_addr, allow_list_key(address))
}

/// `SetAllowListRole` — store `role` for `address` at `precompile_addr`.
///
/// # Errors
/// Returns [`PrecompileError`] on an underlying state-write failure.
pub fn set_allow_list_role(
    state: &mut dyn PrecompileStateOps,
    precompile_addr: Address,
    address: Address,
    role: Role,
) -> Result<(), PrecompileError> {
    state.set_state(precompile_addr, allow_list_key(address), role.word())
}

/// Dispatches the five shared allow-list selectors at `precompile_addr`
/// (`CreateAllowListFunctions`). Returns `None` if `selector` is not an
/// allow-list function (the embedding precompile tries its own selectors
/// next); `setManager` reads as unknown pre-Durango (its activation gate).
pub(crate) fn dispatch_allowlist(
    precompile_addr: Address,
    selector: [u8; 4],
    args: &[u8],
    gas_limit: u64,
    ctx: &PrecompileCtx,
    state: &mut dyn PrecompileStateOps,
) -> Option<Result<InterpreterResult, PrecompileError>> {
    match selector {
        SEL_READ_ALLOW_LIST => Some(read_allow_list(
            precompile_addr,
            args,
            gas_limit,
            ctx,
            state,
        )),
        SEL_SET_ADMIN => Some(set_role(
            precompile_addr,
            Role::Admin,
            args,
            gas_limit,
            ctx,
            state,
        )),
        SEL_SET_ENABLED => Some(set_role(
            precompile_addr,
            Role::Enabled,
            args,
            gas_limit,
            ctx,
            state,
        )),
        SEL_SET_NONE => Some(set_role(
            precompile_addr,
            Role::None,
            args,
            gas_limit,
            ctx,
            state,
        )),
        SEL_SET_MANAGER if ctx.block.is_durango => Some(set_role(
            precompile_addr,
            Role::Manager,
            args,
            gas_limit,
            ctx,
            state,
        )),
        _ => None,
    }
}

/// `createReadAllowList` — read the role word of the input address.
fn read_allow_list(
    precompile_addr: Address,
    args: &[u8],
    gas_limit: u64,
    ctx: &PrecompileCtx,
    state: &mut dyn PrecompileStateOps,
) -> Result<InterpreterResult, PrecompileError> {
    let mut g = Gas::new(gas_limit);
    if !g.record_regular_cost(READ_ALLOW_LIST_GAS) {
        return Ok(out_of_gas(gas_limit));
    }
    // Strict (exact) input length pre-Durango; padded inputs tolerated after.
    if !check_args_len(args, 1, !ctx.block.is_durango) {
        return Ok(failure(gas_limit));
    }
    let Some(read_address) = read_addr(args, 0) else {
        return Ok(failure(gas_limit));
    };
    let word = get_allow_list_word(state, precompile_addr, read_address)?;
    Ok(success(word.to_vec(), g))
}

/// `createAllowListRoleSetter` — set the input address to `role`, allow-list
/// gated, with the Durango+ `RoleSet` event.
fn set_role(
    precompile_addr: Address,
    role: Role,
    args: &[u8],
    gas_limit: u64,
    ctx: &PrecompileCtx,
    state: &mut dyn PrecompileStateOps,
) -> Result<InterpreterResult, PrecompileError> {
    let mut g = Gas::new(gas_limit);
    if !g.record_regular_cost(MODIFY_ALLOW_LIST_GAS) {
        return Ok(out_of_gas(gas_limit));
    }
    if !check_args_len(args, 1, !ctx.block.is_durango) {
        return Ok(failure(gas_limit));
    }
    let Some(modify_address) = read_addr(args, 0) else {
        return Ok(failure(gas_limit));
    };
    if ctx.read_only {
        // Go `vm.ErrWriteProtection`.
        return Ok(failure(gas_limit));
    }
    let caller_status = get_allow_list_status(state, precompile_addr, ctx.caller)?;
    let modify_status = get_allow_list_status(state, precompile_addr, modify_address)?;
    if !caller_status.can_modify(modify_status, role) {
        // Go `ErrCannotModifyAllowList`.
        return Ok(failure(gas_limit));
    }
    if ctx.block.is_durango {
        if !g.record_regular_cost(ALLOW_LIST_EVENT_GAS) {
            return Ok(out_of_gas(gas_limit));
        }
        // RoleSet(uint256 indexed role, address indexed account, address
        // indexed sender) + non-indexed oldRole.
        state.add_log(
            precompile_addr,
            vec![
                B256::from(ROLE_SET_EVENT_TOPIC),
                role.word(),
                B256::new(word_addr(modify_address)),
                B256::new(word_addr(ctx.caller)),
            ],
            modify_status.word().to_vec(),
        );
    }
    set_allow_list_role(state, precompile_addr, modify_address, role)?;
    Ok(success(Vec::new(), g))
}

/// A standalone allow-list precompile (ContractDeployerAllowList /
/// TxAllowList): ONLY the five shared selectors at its address.
#[derive(Clone, Copy, Debug)]
pub struct AllowListPrecompile {
    /// The precompile's fixed contract address.
    address: Address,
}

impl AllowListPrecompile {
    /// An allow-list precompile at `address` (use the
    /// [`DEPLOYER_ALLOW_LIST_ADDRESS`] / [`TX_ALLOW_LIST_ADDRESS`] constants).
    #[must_use]
    pub fn new(address: Address) -> Self {
        Self { address }
    }

    /// The registry module, activated at the upgrade timestamp `activation`.
    #[must_use]
    pub fn module(self, activation: u64) -> PrecompileModule {
        PrecompileModule {
            address: self.address,
            activation,
            precompile: Arc::new(self),
        }
    }
}

impl StatefulPrecompile for AllowListPrecompile {
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
        dispatch_allowlist(self.address, selector, args, gas_limit, ctx, state)
            .unwrap_or_else(|| Ok(failure(gas_limit)))
    }
}

/// Splits the 4-byte selector off `input` (Go `contract.ParseSelector`).
pub(crate) fn split_selector(input: &[u8]) -> Option<([u8; 4], &[u8])> {
    if input.len() < 4 {
        return None;
    }
    let mut selector = [0u8; 4];
    selector.copy_from_slice(&input[0..4]);
    Some((selector, &input[4..]))
}
