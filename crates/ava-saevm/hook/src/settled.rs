// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! [`Settled`] — information about the block settled by a header, plus the
//! gas-clock reconstruction helper (specs/11 §9.1).
//!
//! Port of `vms/saevm/hook/hook.go::{Settled, SettledGasTime}`. Fields refer to
//! post-execution state.

use ava_saevm_gastime::{GasPriceConfig, GasTime};
use ava_vm::components::gas::Gas;

/// Information about the block that is settled by a header. Fields refer to
/// post-execution state.
///
/// Port of Go's `hook.Settled`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settled {
    /// Height of the settled block.
    pub height: u64,
    /// Unix-seconds component of the settled gas clock.
    pub gas_unix: u64,
    /// Sub-second numerator (in gas) of the settled gas clock.
    pub gas_numerator: Gas,
    /// Excess (the `x` variable of ACP-176) at the settled block.
    pub excess: Gas,
}

impl Settled {
    /// Reconstructs the [`GasTime`] associated with the post-execution state of
    /// the settled block, given the gas `target` and `config` in effect.
    ///
    /// Mirrors Go's `hook.SettledGasTime`, which builds
    /// `proxytime.New(GasUnix, GasNumerator, SafeRateOfTarget(target))` and then
    /// `gastime.FromProxyTime(pt, Excess, config)`. The reconstruction uses the
    /// `GasNumerator` as the proxy clock's starting fraction (not zero).
    #[must_use]
    pub fn settled_gas_time(&self, target: Gas, config: GasPriceConfig) -> GasTime {
        GasTime::from_settled(
            self.gas_unix,
            self.gas_numerator.0,
            target.0,
            self.excess.0,
            config,
        )
    }
}
